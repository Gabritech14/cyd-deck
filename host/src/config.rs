use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const DEFAULT_CONFIG: &str = include_str!("../default-config.toml");

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    /// Lingua dell'interfaccia: "auto" o vuoto (quella del sistema), "it" o "en".
    #[serde(skip_serializing_if = "String::is_empty")]
    pub language: String,
    /// Guida rapida già vista nell'editor.
    pub intro_done: bool,
    pub theme: ThemeCfg,
    pub device: Device,
    pub touch: Touch,
    pub layout: Layout,
    pub bar: Bar,
    #[serde(skip_serializing_if = "Wifi::is_empty")]
    pub wifi: Wifi,
    #[serde(skip_serializing_if = "HomeAssistant::is_empty")]
    pub homeassistant: HomeAssistant,
    #[serde(skip_serializing_if = "Printer::is_empty")]
    pub printer: Printer,
    #[serde(rename = "page")]
    pub pages: Vec<Page>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Device {
    /// "auto" cerca la scheda per VID/PID USB, altrimenti un path tipo /dev/ttyACM0.
    pub port: String,
    /// Rotazione del display, 0..=3 (1 e 3 = orizzontale).
    pub rotation: u8,
    pub bgr: bool,
    pub invert: bool,
    /// Secondi senza tocchi prima di spegnere la retroilluminazione (0 = mai).
    pub idle_timeout: u64,
}

impl Default for Device {
    fn default() -> Self {
        Self { port: "auto".into(), rotation: 1, bgr: true, invert: true, idle_timeout: 300 }
    }
}

/// Colori del deck e dell'editor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeCfg {
    /// "auto" = Caelestia se c'è, altrimenti `accent`; "caelestia"; "custom" = sempre `accent`.
    pub style: String,
    /// Colore principale, es. "#ffb878": tutti gli altri si ricavano da questo.
    pub accent: String,
    /// Tema chiaro invece che scuro (solo con `accent`).
    pub light: bool,
}

impl Default for ThemeCfg {
    fn default() -> Self {
        Self { style: "auto".into(), accent: "#ffb878".into(), light: false }
    }
}

/// Barra di stato in cima allo schermo del deck.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Bar {
    pub show: bool,
    /// Nome della pagina, a sinistra.
    pub page: bool,
    pub clock: bool,
    /// Icona del collegamento: USB o Wi-Fi.
    pub link: bool,
    pub battery: bool,
}

impl Default for Bar {
    fn default() -> Self {
        Self { show: true, page: true, clock: true, link: true, battery: true }
    }
}

/// Altezza della barra di stato in pixel (0 se nascosta).
pub fn bar_height(cfg: &Config) -> u32 {
    if cfg.bar.show { 22 } else { 0 }
}

/// Rete Wi-Fi della scheda. Il demone la invia alla scheda quando è collegata via USB;
/// da lì in poi la scheda funziona anche senza cavo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Wifi {
    pub ssid: String,
    /// Sta nel portachiavi: si legge dal file solo per spostarla da una config vecchia.
    #[serde(skip_serializing)]
    pub password: String,
    /// Il portachiavi non ha risposto: la password è sconosciuta e non va inviata alla scheda.
    #[serde(skip)]
    pub locked: bool,
    /// Ultima impronta calcolata con la password: serve a trovare la scheda a portachiavi chiuso.
    /// Non è un segreto, la scheda la annuncia in chiaro sulla rete.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub fingerprint: String,
    /// Codice segreto condiviso con la scheda (generato dall'editor).
    pub token: String,
    /// Indirizzo IP fisso della scheda; vuoto = la cerca da solo sulla rete.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub address: String,
}

impl Wifi {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    pub fn enabled(&self) -> bool {
        !self.ssid.is_empty() && !self.token.is_empty()
    }

    /// Dati da salvare sulla scheda: "ssid\0password\0token\0id" (vuoti = Wi-Fi spento).
    pub fn payload(&self) -> Vec<u8> {
        if !self.enabled() {
            return Vec::new();
        }
        format!("{}\0{}\0{}\0{}", self.ssid, self.password, self.token, self.id()).into_bytes()
    }

    /// Impronta della configurazione, come la riporta la scheda ("-" se non ne ha).
    pub fn id(&self) -> String {
        if !self.enabled() {
            return "-".into();
        }
        if self.locked && !self.fingerprint.is_empty() {
            return self.fingerprint.clone();
        }
        use sha2::{Digest, Sha256};
        let h = Sha256::digest(format!("{}\0{}\0{}", self.ssid, self.password, self.token));
        h[..6].iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Server Home Assistant; il token sta nel portachiavi.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct HomeAssistant {
    /// Es. http://homeassistant.local:8123 (vuoto = non usato).
    pub url: String,
}

impl HomeAssistant {
    fn is_empty(&self) -> bool {
        self.url.is_empty()
    }
}

/// Stampante Bambu Lab in LAN; il codice di accesso sta nel portachiavi.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Printer {
    pub ip: String,
    /// Numero di serie: serve per gli argomenti MQTT e per riconoscere il certificato.
    pub serial: String,
}

impl Printer {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Nuovo codice casuale per l'abbinamento con la scheda.
pub fn new_token() -> String {
    let mut buf = [0u8; 16];
    // Senza casualità vera il codice sarebbe indovinabile: meglio fermarsi.
    getrandom::fill(&mut buf).expect("generatore casuale del sistema non disponibile");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Touch {
    pub swap_xy: bool,
    pub invert_x: bool,
    pub invert_y: bool,
}

impl Default for Touch {
    fn default() -> Self {
        Self { swap_xy: true, invert_x: false, invert_y: true }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    pub cols: u32,
    pub rows: u32,
    pub gap: u32,
    pub radius: f32,
    /// Secondi tra un aggiornamento e l'altro dei tasti con `state`/`label_cmd`.
    pub poll_interval: f32,
    pub icon_font: String,
    pub text_font: String,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            cols: 4,
            rows: 3,
            gap: 8,
            radius: 18.0,
            poll_interval: 2.0,
            icon_font: "Material Symbols Rounded".into(),
            text_font: "Rubik".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Page {
    pub name: String,
    /// "keys" (griglia di tasti, default) oppure "mixer" (volumi delle app e lettore).
    #[serde(skip_serializing_if = "PageKind::is_keys")]
    pub kind: PageKind,
    #[serde(rename = "key", skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<Key>,
    #[serde(skip_serializing_if = "MixerConfig::is_default")]
    pub mixer: MixerConfig,
    /// App (classe della finestra in Hyprland) che aprono questa pagina quando vanno in primo piano.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub auto: Vec<String>,
    /// Pagina nascosta: lo swipe la salta, si apre solo con le app di `auto` (o con un tasto "page:").
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

impl Page {
    /// Se la finestra con questa classe deve aprire la pagina (maiuscole indifferenti, anche parziale).
    pub fn matches_app(&self, class: &str) -> bool {
        let class = class.to_lowercase();
        !class.is_empty() && self.auto.iter().map(|a| a.trim().to_lowercase()).any(|a| !a.is_empty() && class.contains(&a))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PageKind {
    #[default]
    Keys,
    Mixer,
}

impl PageKind {
    fn is_keys(&self) -> bool {
        *self == PageKind::Keys
    }
}

/// Opzioni della pagina audio.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MixerConfig {
    /// Scheda del lettore musicale attivo (quello di Caelestia).
    pub player: bool,
    /// Volume generale (uscita predefinita).
    pub master: bool,
    /// Volume del microfono predefinito.
    pub mic: bool,
    /// Cursori verticali (true) oppure righe orizzontali con il nome accanto.
    pub vertical: bool,
    /// App da non mostrare (nome come appare nel mixer, maiuscole indifferenti).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hide: Vec<String>,
}

impl Default for MixerConfig {
    fn default() -> Self {
        Self { player: true, master: true, mic: false, vertical: true, hide: Vec::new() }
    }
}

impl MixerConfig {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Key {
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Nome di un'icona Material Symbols (es. "volume_up") o un carattere.
    pub icon: Option<String>,
    /// Icona mostrata quando `state` è attivo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_active: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Comando shell da eseguire alla pressione.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exec: Option<String>,
    /// Azione interna: next-page, prev-page, page:<nome>, backlight-off, reload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// Comando shell: exit code 0 = tasto attivo (evidenziato).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Comando shell il cui output sostituisce l'etichetta.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label_cmd: Option<String>,
    /// Macro: passi eseguiti in ordine dopo `exec`.
    #[serde(rename = "step", skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<Step>,
    /// Pausa automatica tra un passo e l'altro della macro, in millisecondi.
    #[serde(skip_serializing_if = "is_zero")]
    pub step_delay: u64,
    /// Parte audio al posto del tasto: cursore del volume o lettore musicale.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub widget: Option<Widget>,
    /// Cursore del volume: "" = volume generale, "@mic" = microfono, altrimenti il nome dell'app.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub target: String,
    /// Dimensioni della parte audio, in tasti (0 o 1 = un tasto).
    #[serde(skip_serializing_if = "is_one")]
    pub w: u32,
    #[serde(skip_serializing_if = "is_one")]
    pub h: u32,
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

fn is_one(v: &u32) -> bool {
    *v <= 1
}

/// Parti audio che si possono mettere nella griglia insieme ai tasti.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Widget {
    Volume,
    Player,
    /// Ora e data.
    Clock,
    /// Limiti d'uso di Claude: sessione di 5 ore e settimana.
    ClaudeLimits,
    /// Stampante 3D: barra di avanzamento, tempo rimanente, strato e temperature.
    Printer,
}

/// Pagine raggiungibili con lo swipe: quelle non nascoste (tutte, se lo sono tutte).
pub fn visible_pages(pages: &[Page]) -> Vec<usize> {
    let v: Vec<usize> = (0..pages.len()).filter(|&i| !pages[i].hidden).collect();
    if v.is_empty() { (0..pages.len()).collect() } else { v }
}

/// Pagina dopo `from` di `delta` posti tra quelle visibili. Da una pagina nascosta si
/// va alla prima visibile dopo (o prima) di lei.
pub fn step_page(pages: &[Page], from: usize, delta: isize) -> usize {
    let v = visible_pages(pages);
    if v.is_empty() {
        return from;
    }
    let n = v.len() as isize;
    match v.iter().position(|&p| p == from) {
        Some(i) => v[(i as isize + delta).rem_euclid(n) as usize],
        None if delta > 0 => v.iter().copied().find(|&p| p > from).unwrap_or(v[0]),
        None => v.iter().rev().copied().find(|&p| p < from).unwrap_or(v[v.len() - 1]),
    }
}

/// Quali finestre chiude il passo "close-windows".
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CloseScope {
    /// Solo quelle del workspace attivo.
    #[default]
    Current,
    /// Quelle di un workspace preciso.
    Workspace,
    /// Tutte.
    All,
}

/// Un passo di una macro.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Step {
    /// Esegue un comando; con `workspace` si va su quel workspace e le finestre si aprono lì.
    Exec {
        exec: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        /// Con `workspace`: apre lì senza spostare la vista.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        background: bool,
        /// Aspetta che il comando finisca prima del passo successivo.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        wait: bool,
    },
    /// Passa a un workspace di Hyprland ("1", "+1", "special:music", ...).
    Workspace { workspace: String },
    /// Pausa prima del passo successivo.
    Delay { ms: u64 },
    /// Azione interna del deck (next-page, page:<nome>, backlight-off, reload).
    Action { action: String },
    /// Chiude le finestre aperte e aspetta che siano sparite.
    CloseWindows {
        #[serde(default)]
        scope: CloseScope,
        /// Workspace da svuotare quando `scope = "workspace"`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        /// Chiude anche le finestre dei workspace speciali (musica, social, ...).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        special: bool,
    },
}

impl Key {
    pub fn is_empty(&self) -> bool {
        self.widget.is_none() && self.icon.is_none() && self.label.is_none() && self.label_cmd.is_none()
    }

    /// Larghezza e altezza in tasti: solo le parti audio possono occuparne più di uno.
    pub fn span(&self) -> (u32, u32) {
        if self.widget.is_some() { (self.w.max(1), self.h.max(1)) } else { (1, 1) }
    }
}

/// Per ogni posto della griglia, il tasto che lo occupa: le parti audio grandi coprono
/// i posti vicini (i tasti che stanno sotto restano nella config ma non si vedono).
pub fn owners(keys: &[Key], cols: u32, rows: u32) -> Vec<usize> {
    let n = (cols * rows) as usize;
    let mut owner: Vec<Option<usize>> = vec![None; n];
    for i in 0..n {
        if owner[i].is_some() {
            continue;
        }
        owner[i] = Some(i);
        let Some(k) = keys.get(i) else { continue };
        let (w, h) = k.span();
        let (c0, r0) = (i as u32 % cols, i as u32 / cols);
        let free = |r: u32, c: u32| owner[(r * cols + c) as usize].is_none_or(|o| o == i);
        // Si restringe se urta il bordo o un'altra parte già piazzata: resta un rettangolo.
        let w = (c0..(c0 + w).min(cols)).take_while(|&c| free(r0, c)).count() as u32;
        let h = (r0..(r0 + h).min(rows)).take_while(|&r| (c0..c0 + w).all(|c| free(r, c))).count() as u32;
        for r in r0..r0 + h {
            for c in c0..c0 + w {
                owner[(r * cols + c) as usize] = Some(i);
            }
        }
    }
    owner.into_iter().enumerate().map(|(i, o)| o.unwrap_or(i)).collect()
}

/// Spazio effettivo (colonne, righe) di un tasto, tenendo conto del bordo della griglia
/// e dei tasti che lo precedono.
pub fn cells(owners: &[usize], i: usize, cols: u32) -> (u32, u32) {
    let (c0, r0) = (i as u32 % cols, i as u32 / cols);
    let mine: Vec<usize> = (0..owners.len()).filter(|&j| owners[j] == i).collect();
    let w = mine.iter().map(|&j| j as u32 % cols).max().map_or(1, |c| c - c0 + 1);
    let h = mine.iter().map(|&j| j as u32 / cols).max().map_or(1, |r| r - r0 + 1);
    (w, h)
}

pub fn config_path() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config").join("cyd-deck/config.toml")
}

pub fn scheme_path() -> PathBuf {
    xdg_dir("XDG_STATE_HOME", ".local/state").join("caelestia/scheme.json")
}

fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var) {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(fallback),
    }
}

/// Scrive la config di default se non esiste ancora.
/// Pagine della config d'esempio nella lingua attuale. Senza Caelestia la sua pagina sparisce,
/// i tasti della musica passano a playerctl e quelli che servono solo a Caelestia restano vuoti.
pub fn sample_pages(caelestia: bool) -> Vec<Page> {
    let def: Config = toml::from_str(DEFAULT_CONFIG).unwrap_or_default();
    let mut pages: Vec<Page> = def.pages.into_iter().filter(|p| caelestia || p.name != "Caelestia").collect();
    let renames: Vec<(String, String)> = pages.iter().map(|p| (p.name.clone(), crate::i18n::t(&p.name).to_string())).collect();
    let first = renames.first().map(|(_, n)| n.clone()).unwrap_or_default();
    for p in &mut pages {
        p.name = crate::i18n::t(&p.name).to_string();
        for k in &mut p.keys {
            if !caelestia {
                if let Some(exec) = k.exec.clone().filter(|e| e.starts_with("caelestia")) {
                    let player = match exec.as_str() {
                        "caelestia shell mpris previous" => Some("playerctl previous"),
                        "caelestia shell mpris playPause" => Some("playerctl play-pause"),
                        "caelestia shell mpris next" => Some("playerctl next"),
                        _ => None,
                    };
                    match player {
                        Some(cmd) => k.exec = Some(cmd.into()),
                        None => *k = Key::default(),
                    }
                }
            }
            if let Some(l) = &mut k.label {
                *l = crate::i18n::t(l).to_string();
            }
            // I tasti "vai alla pagina" seguono il nome tradotto (o la prima pagina, se la loro non c'è).
            if let Some(target) = k.action.as_deref().and_then(|a| a.strip_prefix("page:")) {
                let new = renames.iter().find(|(old, _)| old == target).map_or(first.clone(), |(_, n)| n.clone());
                k.action = Some(format!("page:{new}"));
            }
        }
    }
    pages
}

pub fn ensure_exists(path: &Path) -> Result<()> {
    if !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let caelestia = scheme_path().exists();
        if caelestia && !crate::i18n::english() {
            // Il file d'esempio così com'è, con i suoi commenti.
            std::fs::write(path, DEFAULT_CONFIG)?;
        } else {
            let mut cfg: Config = toml::from_str(DEFAULT_CONFIG)?;
            cfg.pages = sample_pages(caelestia);
            save(path, &cfg)?;
        }
        eprintln!("{}", tr!("creata config di default in {}", path.display()));
    }
    Ok(())
}

const SAVED_HEADER: &str = "\
# CYD Deck - config (written by the editor / scritta dall'editor: `cyd-deck gui`)
# Reloaded automatically on save / ricaricata a ogni salvataggio.
# Key fields / campi di un tasto: icon, icon_active, label, exec, action, state, label_cmd.
# action: next-page | prev-page | page:<name> | backlight-off | reload

";
/// Salva la config; al primo salvataggio tiene una copia dell'originale in config.toml.bak.
pub fn save(path: &Path, cfg: &Config) -> Result<()> {
    let mut cfg = cfg.clone();
    if !cfg.wifi.locked {
        cfg.wifi.fingerprint = if cfg.wifi.enabled() { cfg.wifi.id() } else { String::new() };
    }
    for page in &mut cfg.pages {
        while page.keys.last().is_some_and(|k| *k == Key::default()) {
            page.keys.pop();
        }
    }
    let text = toml::to_string(&cfg).context("serializzazione config")?;
    let backup = path.with_extension("toml.bak");
    if path.exists() && !backup.exists() {
        std::fs::copy(path, &backup)?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, format!("{SAVED_HEADER}{text}"))?;
    // Contiene il codice di abbinamento della scheda: leggibile solo dall'utente.
    if !cfg.wifi.token.is_empty() {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn load(path: &Path) -> Result<Config> {
    let text = std::fs::read_to_string(path).with_context(|| format!("lettura {}", path.display()))?;
    let mut cfg: Config = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    if !cfg.wifi.password.is_empty() {
        // Config vecchia con la password in chiaro: la sposta nel portachiavi e la toglie dal file.
        match crate::secrets::set(crate::secrets::WIFI_PASSWORD, &cfg.wifi.password) {
            Ok(()) => save(path, &cfg)?,
            Err(e) => eprintln!("wifi: password lasciata nella config, {e:#}"),
        }
    } else if !cfg.wifi.ssid.is_empty() {
        // Mai la finestra di sblocco qui: la password serve solo a cambiare rete alla scheda.
        match crate::secrets::get(crate::secrets::WIFI_PASSWORD, crate::secrets::Prompt::Never) {
            Ok(p) => {
                cfg.wifi.password = p.unwrap_or_default();
                if cfg.wifi.enabled() && cfg.wifi.fingerprint != cfg.wifi.id() {
                    save(path, &cfg)?;
                    cfg.wifi.fingerprint = cfg.wifi.id();
                }
            }
            Err(_) => cfg.wifi.locked = true,
        }
    }
    Ok(cfg)
}

/// Aggiorna i valori della sezione [touch] lasciando intatto il resto del file.
pub fn write_touch(path: &Path, t: &Touch) -> Result<bool> {
    let text = std::fs::read_to_string(path)?;
    let mut section = String::new();
    let mut found = 0;
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            section = trimmed.trim_matches(|c| c == '[' || c == ']').trim().to_string();
        }
        let indent = &line[..line.len() - trimmed.len()];
        let replaced = if section == "touch" {
            [("swap_xy", t.swap_xy), ("invert_x", t.invert_x), ("invert_y", t.invert_y)]
                .into_iter()
                .find(|(k, _)| {
                    trimmed.strip_prefix(k).is_some_and(|rest| rest.trim_start().starts_with('='))
                })
                .map(|(k, v)| format!("{indent}{k} = {v}"))
        } else {
            None
        };
        if replaced.is_some() {
            found += 1;
        }
        out.push(replaced.unwrap_or_else(|| line.to_string()));
    }
    if found < 3 {
        return Ok(false);
    }
    std::fs::write(path, out.join("\n") + "\n")?;
    Ok(true)
}

#[cfg(test)]
mod sample_tests {
    use super::*;

    #[test]
    fn sample_pages_work_without_caelestia() {
        for (caelestia, english) in [(false, false), (false, true), (true, true)] {
            crate::i18n::TEST_ENGLISH.with(|c| c.set(Some(english)));
            let pages = sample_pages(caelestia);
            assert!(!pages.is_empty());
            let names: Vec<&str> = pages.iter().map(|p| p.name.as_str()).collect();
            for k in pages.iter().flat_map(|p| &p.keys) {
                if !caelestia {
                    assert!(!k.exec.as_deref().unwrap_or("").contains("caelestia"), "{:?}", k.exec);
                }
                if let Some(target) = k.action.as_deref().and_then(|a| a.strip_prefix("page:")) {
                    assert!(names.contains(&target), "pagina {target} non esiste in {names:?}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_roundtrip() {
        let cfg: Config = toml::from_str(DEFAULT_CONFIG).unwrap();
        let dir = std::env::temp_dir().join(format!("cyd-deck-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        save(&path, &cfg).unwrap();
        let back = load(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        // I tasti vuoti in mezzo alla griglia devono restare al loro posto.
        assert_eq!(back, cfg);
        assert!(back.pages[2].keys[9] == Key::default());
    }

    #[test]
    fn widget_spans() {
        let vol = |w, h| Key { widget: Some(Widget::Volume), w, h, ..Key::default() };
        // 4x3: volume alto 3 nella prima colonna, lettore largo 3 in alto a destra.
        let mut keys = vec![Key::default(); 12];
        keys[0] = vol(1, 3);
        keys[1] = Key { widget: Some(Widget::Player), w: 3, ..Key::default() };
        keys[5] = vol(2, 2);
        let o = owners(&keys, 4, 3);
        assert_eq!(o, vec![0, 1, 1, 1, 0, 5, 5, 7, 0, 5, 5, 11]);
        assert_eq!((cells(&o, 0, 4), cells(&o, 1, 4), cells(&o, 5, 4), cells(&o, 7, 4)), ((1, 3), (3, 1), (2, 2), (1, 1)));
        // Si ferma contro una parte già piazzata e contro il bordo.
        let mut keys = vec![Key::default(); 12];
        keys[1] = vol(1, 2);
        keys[4] = vol(2, 2);
        keys[3] = vol(3, 1);
        let o = owners(&keys, 4, 3);
        assert_eq!((cells(&o, 4, 4), cells(&o, 3, 4)), ((1, 2), (1, 1)));
        // Un tasto normale resta sempre di un posto.
        assert_eq!(Key { w: 3, h: 3, ..Key::default() }.span(), (1, 1));
    }

    #[test]
    fn hidden_pages() {
        let page = |hidden| Page { hidden, ..Page::default() };
        let pages = vec![page(false), page(true), page(false), page(true)];
        assert_eq!(visible_pages(&pages), vec![0, 2]);
        assert_eq!((step_page(&pages, 0, 1), step_page(&pages, 2, 1), step_page(&pages, 0, -1)), (2, 0, 2));
        // Da una pagina nascosta (aperta da un'app) lo swipe porta alle visibili vicine.
        assert_eq!((step_page(&pages, 1, 1), step_page(&pages, 1, -1), step_page(&pages, 3, 1)), (2, 0, 0));
        assert_eq!(visible_pages(&[page(true), page(true)]), vec![0, 1]);
    }

    #[test]
    fn auto_pages() {
        let p = Page { name: "Musica".into(), auto: vec!["spotify".into(), " Minecraft ".into(), "".into()], ..Page::default() };
        assert!(p.matches_app("Spotify"));
        assert!(p.matches_app("Minecraft* 1.21.4"));
        assert!(!p.matches_app("foot"));
        assert!(!p.matches_app(""));
        let mut cfg = Config::default();
        cfg.layout.rows = 3;
        let g = crate::app::Grid::new(&cfg, 320, 240);
        assert!(g.y0 >= bar_height(&cfg) && g.y0 + 3 * g.size + 2 * cfg.layout.gap <= 240);
    }

    #[test]
    fn steps_roundtrip() {
        let mut cfg = Config::default();
        let key = Key {
            icon: Some("rocket_launch".into()),
            steps: vec![
                Step::Workspace { workspace: "1".into() },
                Step::Exec { exec: "sklauncher".into(), workspace: Some("1".into()), background: false, wait: false },
                Step::Delay { ms: 500 },
                Step::Exec { exec: "caelestia shell gameMode enable".into(), workspace: Some("2".into()), background: true, wait: true },
                Step::Action { action: "next-page".into() },
                Step::CloseWindows { scope: CloseScope::All, workspace: None, special: true },
                Step::CloseWindows { scope: CloseScope::Workspace, workspace: Some("3".into()), special: false },
            ],
            step_delay: 250,
            ..Key::default()
        };
        cfg.pages.push(Page { name: "Giochi".into(), keys: vec![Key::default(), key], ..Page::default() });
        cfg.pages.push(Page {
            name: "Audio".into(),
            kind: PageKind::Mixer,
            mixer: MixerConfig { mic: true, hide: vec!["Discord".into()], ..MixerConfig::default() },
            ..Page::default()
        });
        let text = toml::to_string(&cfg).unwrap();
        assert!(text.contains("[[page.key.step]]"), "{text}");
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back, cfg);
    }
}
