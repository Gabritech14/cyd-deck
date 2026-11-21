//! Home Assistant via API REST. I tasti usano comandi normali (`cyd-deck ha toggle light.salotto`):
//! il demone li riconosce e li esegue da sé, con il token tenuto in memoria; da terminale
//! passano dal sottocomando `cyd-deck ha`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

use crate::config::{self, Config};
use crate::presets::Plugin;
use crate::secrets;

pub const USAGE: &str = "\
uso: cyd-deck ha <comando> [entità]
  check            prova il collegamento a Home Assistant
  list [testo]     elenca le entità (filtrate per testo) con il loro stato
  toggle <entità>  accende/spegne (luci, prese, tapparelle, lettori...)
  on | off <entità>
  run <entità>     scena, script, pulsante o automazione
  is-on <entità>   esce con 0 se è acceso/aperto/in riproduzione
  state <entità>   stampa lo stato (con l'unità di misura)";

#[derive(Debug, Clone, Deserialize)]
pub struct Entity {
    pub entity_id: String,
    pub state: String,
    #[serde(default)]
    pub attributes: serde_json::Value,
}

impl Entity {
    pub fn domain(&self) -> &str {
        self.entity_id.split('.').next().unwrap_or_default()
    }

    pub fn name(&self) -> String {
        self.attributes["friendly_name"].as_str().map_or_else(|| self.entity_id.clone(), str::to_string)
    }

    pub fn device_class(&self) -> &str {
        self.attributes["device_class"].as_str().unwrap_or_default()
    }

    pub fn is_on(&self) -> bool {
        matches!(self.state.as_str(), "on" | "open" | "opening" | "playing" | "home")
    }

    /// Stato da mostrare su un tasto: numeri arrotondati, unità di misura, parole in italiano.
    pub fn display(&self) -> String {
        let unit = self.attributes["unit_of_measurement"].as_str().unwrap_or_default();
        let value = match self.state.as_str() {
            "on" => "acceso".into(),
            "off" => "spento".into(),
            "open" => "aperto".into(),
            "closed" => "chiuso".into(),
            "home" => "a casa".into(),
            "not_home" => "fuori".into(),
            "unavailable" | "unknown" => return "n/d".into(),
            s => match s.parse::<f64>() {
                Ok(v) if v.fract() != 0.0 => format!("{v:.1}"),
                _ => s.to_string(),
            },
        };
        match unit {
            "" => value,
            "%" | "°C" | "°F" => format!("{value}{unit}"),
            u => format!("{value} {u}"),
        }
    }
}

/// Comando per Home Assistant scritto in un tasto.
#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    Check,
    List(String),
    Toggle(String),
    On(String),
    Off(String),
    Run(String),
    IsOn(String),
    State(String),
}

impl Cmd {
    pub fn from_args(args: &[&str]) -> Option<Cmd> {
        let id = || args.get(1).filter(|e| e.contains('.')).map(|e| e.to_string());
        Some(match *args.first()? {
            "check" => Cmd::Check,
            "list" => Cmd::List(args.get(1).unwrap_or(&"").to_lowercase()),
            "toggle" => Cmd::Toggle(id()?),
            "on" => Cmd::On(id()?),
            "off" => Cmd::Off(id()?),
            "run" => Cmd::Run(id()?),
            "is-on" => Cmd::IsOn(id()?),
            "state" => Cmd::State(id()?),
            _ => return None,
        })
    }

    /// Riconosce `cyd-deck ha ...` scritto da solo in un tasto (niente pipe o altri comandi).
    pub fn parse(shell: &str) -> Option<Cmd> {
        let words: Vec<&str> = shell.split_whitespace().collect();
        let rest = match words.as_slice() {
            ["cyd-deck", "ha", rest @ ..] => rest,
            _ => return None,
        };
        if rest.len() > 2 {
            return None;
        }
        Cmd::from_args(rest)
    }

    /// Comando shell equivalente, da salvare nei tasti.
    pub fn shell(&self) -> String {
        let (verb, id) = match self {
            Cmd::Check => return "cyd-deck ha check".into(),
            Cmd::List(q) => return format!("cyd-deck ha list {q}").trim_end().into(),
            Cmd::Toggle(e) => ("toggle", e),
            Cmd::On(e) => ("on", e),
            Cmd::Off(e) => ("off", e),
            Cmd::Run(e) => ("run", e),
            Cmd::IsOn(e) => ("is-on", e),
            Cmd::State(e) => ("state", e),
        };
        format!("cyd-deck ha {verb} {id}")
    }

    fn entity(&self) -> Option<&str> {
        match self {
            Cmd::Check | Cmd::List(_) => None,
            Cmd::Toggle(e) | Cmd::On(e) | Cmd::Off(e) | Cmd::Run(e) | Cmd::IsOn(e) | Cmd::State(e) => Some(e),
        }
    }
}

pub struct Client {
    url: String,
    token: String,
    agent: ureq::Agent,
}

impl Client {
    /// Dalla config e dal portachiavi.
    pub fn from_config(cfg: &Config, prompt: secrets::Prompt) -> Result<Client> {
        let url = cfg.homeassistant.url.trim().trim_end_matches('/').to_string();
        if url.is_empty() {
            bail!("{}", tr!("indirizzo di Home Assistant non impostato (editor → Impostazioni)"));
        }
        let token = secrets::get(secrets::HA_TOKEN, prompt)?.ok_or_else(|| anyhow!("{}", tr!("token di Home Assistant non salvato (editor → Impostazioni)")))?;
        Ok(Client::new(url, token))
    }

    pub fn new(url: String, token: String) -> Client {
        let agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(5))).http_status_as_error(false).build().into();
        Client { url, token, agent }
    }

    fn get(&self, path: &str) -> Result<ureq::http::Response<ureq::Body>> {
        let r = self
            .agent
            .get(format!("{}{path}", self.url))
            .header("Authorization", format!("Bearer {}", self.token))
            .call()
            .with_context(|| tr!("Home Assistant non raggiungibile su {}", self.url))?;
        check_status(r.status().as_u16())?;
        Ok(r)
    }

    /// Nome della casa e versione di Home Assistant.
    pub fn check(&self) -> Result<String> {
        let v: serde_json::Value = self.get("/api/config")?.body_mut_json()?;
        Ok(format!("{} (Home Assistant {})", v["location_name"].as_str().unwrap_or("casa"), v["version"].as_str().unwrap_or("?")))
    }

    pub fn states(&self) -> Result<Vec<Entity>> {
        self.get("/api/states")?.body_mut_json()
    }

    pub fn state(&self, id: &str) -> Result<Entity> {
        self.get(&format!("/api/states/{id}"))?.body_mut_json()
    }

    pub fn call(&self, domain: &str, service: &str, id: &str) -> Result<()> {
        let r = self
            .agent
            .post(format!("{}/api/services/{domain}/{service}", self.url))
            .header("Authorization", format!("Bearer {}", self.token))
            .send_json(serde_json::json!({ "entity_id": id }))
            .with_context(|| tr!("Home Assistant non raggiungibile su {}", self.url))?;
        check_status(r.status().as_u16())
    }

    /// Esegue un comando che agisce su un'entità.
    pub fn act(&self, cmd: &Cmd) -> Result<()> {
        let id = cmd.entity().unwrap_or_default();
        let domain = id.split('.').next().unwrap_or_default();
        let service = match (cmd, domain) {
            (Cmd::Toggle(_), "media_player") => "media_play_pause",
            (Cmd::Toggle(_), "scene" | "script") => "turn_on",
            (Cmd::Toggle(_), "button" | "input_button") => "press",
            (Cmd::Toggle(_), _) => "toggle",
            (Cmd::On(_), "cover") => "open_cover",
            (Cmd::Off(_), "cover") => "close_cover",
            (Cmd::On(_), _) => "turn_on",
            (Cmd::Off(_), _) => "turn_off",
            (Cmd::Run(_), "button" | "input_button") => "press",
            (Cmd::Run(_), "automation") => "trigger",
            (Cmd::Run(_), _) => "turn_on",
            _ => bail!("'{}' non è un'azione", cmd.shell()),
        };
        self.call(domain, service, id)
    }
}

trait JsonBody {
    fn body_mut_json<T: serde::de::DeserializeOwned>(self) -> Result<T>;
}

impl JsonBody for ureq::http::Response<ureq::Body> {
    fn body_mut_json<T: serde::de::DeserializeOwned>(mut self) -> Result<T> {
        self.body_mut().with_config().limit(32 << 20).read_json().context(tr!("risposta di Home Assistant non valida"))
    }
}

fn valid_id(id: &str) -> bool {
    let ok = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    id.split_once('.').is_some_and(|(d, n)| ok(d) && ok(n))
}

fn check_status(code: u16) -> Result<()> {
    match code {
        200..=299 => Ok(()),
        401 | 403 => bail!("{}", tr!("Home Assistant ha rifiutato il token (scaduto o sbagliato?)")),
        404 => bail!("{}", tr!("entità o servizio inesistente in Home Assistant")),
        c => bail!("{}", tr!("Home Assistant ha risposto con errore {}", c)),
    }
}

/// Scheda dello store per un'entità; None per i tipi che su un tasto non servono.
pub fn plugin(e: &Entity) -> Option<Plugin> {
    // L'ID finisce in un comando della shell: si accetta solo la forma di Home Assistant
    // (`dominio.nome`, lettere minuscole, cifre e trattini bassi), mai caratteri speciali.
    if !valid_id(&e.entity_id) {
        return None;
    }
    let id = e.entity_id.clone();
    let toggle = |icon: &str, cat: &str| Plugin {
        icon: icon.into(),
        category: cat.into(),
        exec: Cmd::Toggle(id.clone()).shell(),
        state: Some(Cmd::IsOn(id.clone()).shell()),
        ..Plugin::default()
    };
    let run = |icon: &str, cat: &str| Plugin { icon: icon.into(), category: cat.into(), exec: Cmd::Run(id.clone()).shell(), ..Plugin::default() };
    let show = |icon: &str, cat: &str| Plugin {
        icon: icon.into(),
        category: cat.into(),
        label_cmd: Some(Cmd::State(id.clone()).shell()),
        ..Plugin::default()
    };
    let mut p = match e.domain() {
        "light" => toggle("lightbulb", "HA · Luci"),
        "switch" | "input_boolean" => toggle(if e.device_class() == "outlet" { "power" } else { "toggle_on" }, "HA · Interruttori"),
        "fan" => toggle("mode_fan", "HA · Interruttori"),
        "cover" => toggle("blinds", "HA · Tapparelle"),
        "media_player" => Plugin { icon_active: Some("pause".into()), ..toggle("play_arrow", "HA · Media") },
        "scene" => run("palette", "HA · Scene e script"),
        "script" => run("play_circle", "HA · Scene e script"),
        "button" | "input_button" => run("radio_button_checked", "HA · Scene e script"),
        "automation" => toggle("smart_toy", "HA · Automazioni"),
        "sensor" => show(
            match e.device_class() {
                "temperature" => "thermostat",
                "humidity" => "humidity_percentage",
                "power" | "energy" | "voltage" | "current" => "bolt",
                "battery" => "battery_full",
                "illuminance" => "light_mode",
                _ => "sensors",
            },
            "HA · Sensori",
        ),
        "binary_sensor" => Plugin { state: Some(Cmd::IsOn(id.clone()).shell()), ..show("sensors", "HA · Sensori") },
        "person" | "device_tracker" => show("person", "HA · Persone"),
        _ => return None,
    };
    p.category = crate::i18n::t(&p.category).to_string();
    p.name = e.name();
    p.label = p.name.clone();
    p.description = format!("{id} · {}", e.display());
    Some(p)
}

/// Client del processo, creato al primo uso; il demone lo azzera quando la config cambia.
static CLIENT: Mutex<Option<std::sync::Arc<Client>>> = Mutex::new(None);

pub fn reset() {
    *CLIENT.lock().unwrap() = None;
}

fn client() -> Result<std::sync::Arc<Client>> {
    let mut c = CLIENT.lock().unwrap();
    if let Some(c) = c.as_ref() {
        return Ok(c.clone());
    }
    let cfg = config::load(&config::config_path())?;
    let new = std::sync::Arc::new(Client::from_config(&cfg, secrets::Prompt::Auto)?);
    *c = Some(new.clone());
    Ok(new)
}

/// Esegue la pressione di un tasto in un thread, senza bloccare il demone.
pub fn spawn(cmd: Cmd) {
    std::thread::spawn(move || {
        if let Err(e) = client().and_then(|c| c.act(&cmd)) {
            // Al prossimo giro rilegge config e token (magari appena cambiati).
            reset();
            eprintln!("home assistant: {}: {e:#}", cmd.shell());
        }
    });
}

/// Stati letti una volta per giro di aggiornamento dei tasti.
#[derive(Default)]
pub struct Snapshot {
    states: Option<HashMap<String, Entity>>,
}

impl Snapshot {
    /// Risposta a un comando di stato o di etichetta, come se fosse un comando shell:
    /// (riuscito, testo stampato). None se Home Assistant non risponde.
    pub fn answer(&mut self, cmd: &Cmd) -> Option<(bool, String)> {
        if self.states.is_none() {
            let list = client().and_then(|c| c.states());
            if let Err(e) = &list {
                reset();
                eprintln!("home assistant: {e:#}");
            }
            self.states = Some(list.ok()?.into_iter().map(|e| (e.entity_id.clone(), e)).collect());
        }
        let e = self.states.as_ref()?.get(cmd.entity()?)?;
        Some(match cmd {
            Cmd::IsOn(_) => (e.is_on(), String::new()),
            Cmd::State(_) => (true, e.display()),
            _ => return None,
        })
    }
}

/// `cyd-deck ha ...` da terminale.
pub fn cli(args: &[String]) -> Result<()> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let Some(cmd) = Cmd::from_args(&args) else {
        println!("{USAGE}");
        return Ok(());
    };
    let c = client()?;
    match &cmd {
        Cmd::Check => println!("collegato a {}", c.check()?),
        Cmd::List(q) => {
            let mut all = c.states()?;
            all.sort_by(|a, b| a.entity_id.cmp(&b.entity_id));
            for e in all.iter().filter(|e| q.is_empty() || e.entity_id.contains(q.as_str()) || e.name().to_lowercase().contains(q.as_str())) {
                println!("{:<50} {:<28} {}", e.entity_id, e.display(), e.name());
            }
        }
        Cmd::IsOn(id) => {
            if !c.state(id)?.is_on() {
                std::process::exit(1);
            }
        }
        Cmd::State(id) => println!("{}", c.state(id)?.display()),
        _ => c.act(&cmd)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_key_commands() {
        assert_eq!(Cmd::parse("cyd-deck ha toggle light.salotto"), Some(Cmd::Toggle("light.salotto".into())));
        assert_eq!(Cmd::parse("  cyd-deck  ha is-on switch.presa "), Some(Cmd::IsOn("switch.presa".into())));
        assert_eq!(Cmd::parse("cyd-deck ha toggle light.a && foot"), None);
        assert_eq!(Cmd::parse("cyd-deck ha toggle"), None);
        assert_eq!(Cmd::parse("foot"), None);
        let c = Cmd::Run("scene.cinema".into());
        assert_eq!(Cmd::parse(&c.shell()), Some(c));
    }

    #[test]
    fn displays_states() {
        let e = |state: &str, attrs: serde_json::Value| Entity { entity_id: "sensor.x".into(), state: state.into(), attributes: attrs };
        assert_eq!(e("21.456", serde_json::json!({"unit_of_measurement": "°C"})).display(), "21.5°C");
        assert_eq!(e("350", serde_json::json!({"unit_of_measurement": "W"})).display(), "350 W");
        assert_eq!(e("on", serde_json::json!({})).display(), "acceso");
        assert_eq!(e("unavailable", serde_json::json!({"unit_of_measurement": "W"})).display(), "n/d");
        assert!(e("playing", serde_json::json!({})).is_on());
    }

    #[test]
    fn rejects_shell_in_entity_ids() {
        assert!(valid_id("light.salotto_2"));
        for bad in ["light.x; rm -rf ~", "light.$(id)", "light", ".x", "light.a b", "Light.x", "light.x`y`"] {
            assert!(!valid_id(bad), "{bad}");
        }
        let e = Entity { entity_id: "light.x;reboot".into(), state: "on".into(), attributes: serde_json::json!({}) };
        assert!(plugin(&e).is_none());
    }
}
