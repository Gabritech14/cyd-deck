//! Stampante Bambu Lab (A1 mini) in LAN: MQTT su TLS, porta 8883, utente "bblp" e codice di
//! accesso LAN (nel portachiavi). Il demone tiene aperta una connessione e conserva l'ultimo
//! stato; i tasti usano comandi come `cyd-deck printer progress`, risposti da quello stato.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use serde_json::{Map, Value, json};

use crate::config::{self, Config};
use crate::secrets;

pub const USAGE: &str = "\
uso: cyd-deck printer <comando>
  status                 riassunto dello stato (si collega, aspetta i dati e stampa)
  pause | resume         mette in pausa / riprende la stampa
  stop                   ferma la stampa (da un tasto va premuto due volte entro 3 s)
  light                  accende/spegne la luce
  progress | remaining | temps | layer | state | file   testo per l'etichetta di un tasto
  is-printing | is-paused | is-light                    esce con 0 se vero";

/// Comando per la stampante scritto in un tasto.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cmd {
    Status,
    Pause,
    Resume,
    Stop,
    Light,
    Progress,
    Remaining,
    Temps,
    Layer,
    State,
    File,
    IsPrinting,
    IsPaused,
    IsLight,
}

impl Cmd {
    const ALL: [(&str, Cmd); 14] = [
        ("status", Cmd::Status),
        ("pause", Cmd::Pause),
        ("resume", Cmd::Resume),
        ("stop", Cmd::Stop),
        ("light", Cmd::Light),
        ("progress", Cmd::Progress),
        ("remaining", Cmd::Remaining),
        ("temps", Cmd::Temps),
        ("layer", Cmd::Layer),
        ("state", Cmd::State),
        ("file", Cmd::File),
        ("is-printing", Cmd::IsPrinting),
        ("is-paused", Cmd::IsPaused),
        ("is-light", Cmd::IsLight),
    ];

    pub fn from_arg(arg: &str) -> Option<Cmd> {
        Self::ALL.iter().find(|(n, _)| *n == arg).map(|(_, c)| *c)
    }

    /// Riconosce `cyd-deck printer ...` scritto da solo in un tasto.
    pub fn parse(shell: &str) -> Option<Cmd> {
        match shell.split_whitespace().collect::<Vec<_>>().as_slice() {
            ["cyd-deck", "printer", arg] => Cmd::from_arg(arg),
            _ => None,
        }
    }

    #[cfg(test)]
    pub fn shell(self) -> String {
        let name = Self::ALL.iter().find(|(_, c)| *c == self).map_or("status", |(n, _)| n);
        format!("cyd-deck printer {name}")
    }
}

/// Ultimo stato riportato dalla stampante (il blocco "print" dei messaggi, fuso man mano).
#[derive(Default, Clone)]
pub struct Status {
    pub online: bool,
    pub print: Map<String, Value>,
}

impl Status {
    fn num(&self, k: &str) -> f64 {
        self.print.get(k).and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))).unwrap_or(0.0)
    }

    fn text(&self, k: &str) -> &str {
        self.print.get(k).and_then(Value::as_str).unwrap_or_default()
    }

    /// IDLE, PREPARE, RUNNING, PAUSE, FINISH, FAILED.
    pub fn gcode_state(&self) -> &str {
        self.text("gcode_state")
    }

    pub fn printing(&self) -> bool {
        matches!(self.gcode_state(), "RUNNING" | "PREPARE" | "SLICING")
    }

    pub fn paused(&self) -> bool {
        self.gcode_state() == "PAUSE"
    }

    pub fn active(&self) -> bool {
        self.printing() || self.paused()
    }

    pub fn percent(&self) -> u8 {
        self.num("mc_percent").clamp(0.0, 100.0) as u8
    }

    pub fn light(&self) -> bool {
        self.print.get("lights_report").and_then(Value::as_array).is_some_and(|l| {
            l.iter().any(|x| x["node"] == "chamber_light" && x["mode"] == "on")
        })
    }

    pub fn state_it(&self) -> &'static str {
        if !self.online {
            return "offline";
        }
        match self.gcode_state() {
            "RUNNING" => "in stampa",
            "PREPARE" | "SLICING" => "preparazione",
            "PAUSE" => "in pausa",
            "FINISH" => "finita",
            "FAILED" => "errore",
            _ => "pronta",
        }
    }

    pub fn remaining(&self) -> String {
        let m = self.num("mc_remaining_time") as u32;
        if !self.active() {
            return "--".into();
        }
        if m >= 60 { format!("{}h {:02}m", m / 60, m % 60) } else { format!("{m} min") }
    }

    /// Testo per un comando di etichetta.
    pub fn answer(&self, cmd: Cmd) -> String {
        match cmd {
            Cmd::Progress if self.active() => format!("{}%", self.percent()),
            Cmd::Progress | Cmd::State => self.state_it().into(),
            Cmd::Remaining => self.remaining(),
            Cmd::Temps => format!("{:.0}° / {:.0}°", self.num("nozzle_temper"), self.num("bed_temper")),
            Cmd::Layer if self.active() => format!("{}/{}", self.num("layer_num"), self.num("total_layer_num")),
            Cmd::Layer => "--".into(),
            Cmd::File => self.text("subtask_name").to_string(),
            _ => String::new(),
        }
    }

    pub fn is(&self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::IsPrinting => self.printing(),
            Cmd::IsPaused => self.paused(),
            Cmd::IsLight => self.light(),
            _ => false,
        }
    }

    pub fn summary(&self) -> String {
        let mut s = format!("stato: {}  ·  temperature ugello/piatto: {}", self.state_it(), self.answer(Cmd::Temps));
        if self.active() {
            s += &format!(
                "\n{}  ·  {}%  ·  strato {}  ·  mancano {}",
                self.text("subtask_name"),
                self.percent(),
                self.answer(Cmd::Layer),
                self.remaining()
            );
        }
        s += &format!("\nluce: {}", if self.light() { "accesa" } else { "spenta" });
        s
    }
}

// ---------------------------------------------------------------- MQTT minimo (3.1.1, QoS 0)

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend((s.len() as u16).to_be_bytes());
    out.extend(s.as_bytes());
}

fn packet(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![kind];
    let mut n = body.len();
    loop {
        let mut b = (n % 128) as u8;
        n /= 128;
        if n > 0 {
            b |= 0x80;
        }
        out.push(b);
        if n == 0 {
            break;
        }
    }
    out.extend(body);
    out
}

fn connect_packet(user: &str, pass: &str) -> Vec<u8> {
    let mut b = Vec::new();
    put_str(&mut b, "MQTT");
    b.extend([4, 0xC2, 0, 60]); // livello 4, utente+password+clean session, keepalive 60 s
    put_str(&mut b, &format!("cyd-deck-{}", std::process::id()));
    put_str(&mut b, user);
    put_str(&mut b, pass);
    packet(0x10, &b)
}

fn subscribe_packet(topic: &str) -> Vec<u8> {
    let mut b = vec![0, 1];
    put_str(&mut b, topic);
    b.push(0);
    packet(0x82, &b)
}

fn publish_packet(topic: &str, payload: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    put_str(&mut b, topic);
    b.extend(payload);
    packet(0x30, &b)
}

/// Estrae un pacchetto completo dall'inizio del buffer: (tipo, corpo).
fn take_packet(buf: &mut Vec<u8>) -> Option<(u8, Vec<u8>)> {
    let (mut len, mut mult, mut i) = (0usize, 1usize, 1usize);
    loop {
        let b = *buf.get(i)?;
        len += (b & 0x7f) as usize * mult;
        mult *= 128;
        i += 1;
        if b & 0x80 == 0 {
            break;
        }
        if i > 4 {
            buf.clear();
            return None;
        }
    }
    if buf.len() < i + len {
        return None;
    }
    let kind = buf[0];
    let body = buf[i..i + len].to_vec();
    buf.drain(..i + len);
    Some((kind, body))
}

/// Il certificato della stampante è firmato dalla CA di Bambu, non da una pubblica: si accetta
/// se riporta il numero di serie atteso e, dalla seconda volta, solo se la chiave è la stessa
/// della prima connessione riuscita (come SSH). Così nessuno sulla rete può fingersi la
/// stampante e farsi mandare il codice di accesso.
#[derive(Debug)]
struct PrinterCert {
    serial: String,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

/// Un elemento DER: (tag, contenuto, resto).
fn der(d: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, d) = d.split_first()?;
    let (&first, mut d) = d.split_first()?;
    let len = if first & 0x80 == 0 {
        first as usize
    } else {
        let n = (first & 0x7f) as usize;
        if n == 0 || n > 4 || d.len() < n {
            return None;
        }
        let len = d[..n].iter().fold(0usize, |a, &b| a << 8 | b as usize);
        d = &d[n..];
        len
    };
    (d.len() >= len).then(|| (tag, &d[..len], &d[len..]))
}

/// La chiave pubblica (SubjectPublicKeyInfo) di un certificato, anche X.509 v1 come quello
/// delle stampanti Bambu, che webpki non accetta.
fn spki_of(cert: &[u8]) -> Option<&[u8]> {
    let (_, cert, _) = der(cert)?;
    let (_, mut tbs, _) = der(cert)?;
    // La versione ([0]) c'è solo dalla v2 in su.
    if der(tbs)?.0 == 0xA0 {
        tbs = der(tbs)?.2;
    }
    // Numero di serie, algoritmo, emittente, validità, soggetto.
    for _ in 0..5 {
        tbs = der(tbs)?.2;
    }
    let rest = der(tbs)?.2;
    Some(&tbs[..tbs.len() - rest.len()])
}

impl PrinterCert {
    /// Firma dell'handshake controllata con la chiave del certificato.
    fn verify(
        &self,
        msg: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        let bad = |m: &str| rustls::Error::General(m.into());
        let spki = spki_of(cert.as_ref()).ok_or_else(|| bad("certificato della stampante illeggibile"))?;
        let spki = rustls::pki_types::SubjectPublicKeyInfoDer::from(spki);
        let key = webpki::RawPublicKeyEntity::try_from(&spki).map_err(|e| bad(&format!("chiave della stampante: {e:?}")))?;
        let algs = self.provider.signature_verification_algorithms.mapping.iter().find(|(s, _)| *s == dss.scheme).map(|(_, a)| *a).unwrap_or(&[]);
        if algs.iter().any(|alg| key.verify_signature(*alg, msg, dss.signature()).is_ok()) {
            // La chiave ha appena dimostrato di essere vera: alla prima volta la si ricorda.
            if pinned_key(&self.serial).is_none() {
                save_pin(&self.serial, spki.as_ref());
            }
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        } else {
            Err(bad("firma della stampante non valida"))
        }
    }
}

impl rustls::client::danger::ServerCertVerifier for PrinterCert {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let der = end_entity.as_ref();
        if let Some(pin) = pinned_key(&self.serial) {
            let now = spki_of(der).map(key_hash);
            if now.as_deref() != Some(pin.as_str()) {
                return Err(rustls::Error::General(format!(
                    "la chiave della stampante è cambiata: se l'hai sostituita o resettata cancella {}",
                    pin_path().display()
                )));
            }
        }
        if !self.serial.is_empty() && der.windows(self.serial.len()).any(|w| w == self.serial.as_bytes()) {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("il certificato non è della stampante configurata".into()))
        }
    }

    fn verify_tls12_signature(
        &self,
        msg: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.verify(msg, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        msg: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.verify(msg, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

fn pin_path() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share/cyd-deck/printer-keys")
}

fn key_hash(spki: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(spki).iter().map(|b| format!("{b:02x}")).collect()
}

/// Impronta della chiave ricordata per questa stampante (righe "seriale impronta").
fn pinned_key(serial: &str) -> Option<String> {
    let text = std::fs::read_to_string(pin_path()).ok()?;
    text.lines().find_map(|l| {
        let (s, h) = l.split_once(' ')?;
        (s == serial).then(|| h.trim().to_string())
    })
}

fn save_pin(serial: &str, spki: &[u8]) {
    let path = pin_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    text.push_str(&format!("{serial} {}\n", key_hash(spki)));
    let _ = std::fs::write(&path, text);
}

type Tls = rustls::StreamOwned<rustls::ClientConnection, TcpStream>;

fn open_tls(ip: &str, serial: &str) -> Result<Tls> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(PrinterCert { serial: serial.into(), provider: provider.clone() });
    let cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    let name = ServerName::try_from(ip.to_string()).map_err(|_| anyhow!("indirizzo non valido: {ip}"))?;
    let conn = rustls::ClientConnection::new(Arc::new(cfg), name)?;
    let tcp = TcpStream::connect_timeout(&format!("{ip}:8883").parse()?, Duration::from_secs(5))
        .with_context(|| format!("stampante non raggiungibile su {ip}"))?;
    tcp.set_read_timeout(Some(Duration::from_millis(500)))?;
    Ok(rustls::StreamOwned::new(conn, tcp))
}

/// Connessione viva alla stampante.
struct Handle {
    status: Arc<Mutex<Status>>,
    tx: Sender<Value>,
    alive: Arc<AtomicBool>,
}

static HANDLE: Mutex<Option<Handle>> = Mutex::new(None);
/// Ultimo tentativo fallito di avvio (per non chiedere il codice a raffica).
static LAST_FAIL: Mutex<Option<Instant>> = Mutex::new(None);
static LAST_STOP: Mutex<Option<Instant>> = Mutex::new(None);

/// Una sessione MQTT: legge i messaggi e manda i comandi finché la connessione regge.
fn session(ip: &str, serial: &str, code: &str, status: &Mutex<Status>, rx: &Receiver<Value>) -> Result<()> {
    let mut tls = open_tls(ip, serial)?;
    tls.write_all(&connect_packet("bblp", code))?;
    let mut buf = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut chunk = [0u8; 16 * 1024];
    let mut connected = false;
    let report = format!("device/{serial}/report");
    let request = format!("device/{serial}/request");
    let mut last_ping = Instant::now();
    let mut last_rx = Instant::now();
    loop {
        match tls.read(&mut chunk) {
            Ok(0) => bail!("la stampante ha chiuso la connessione"),
            Ok(n) => {
                buf.extend(&chunk[..n]);
                last_rx = Instant::now();
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(e) => return Err(e.into()),
        }
        while let Some((kind, body)) = take_packet(&mut buf) {
            match kind >> 4 {
                2 => {
                    match body.get(1) {
                        Some(0) => {}
                        Some(4 | 5) => bail!("codice di accesso LAN rifiutato"),
                        rc => bail!("connessione rifiutata dalla stampante ({rc:?})"),
                    }
                    connected = true;
                    tls.write_all(&subscribe_packet(&report))?;
                    let pushall = json!({"pushing": {"sequence_id": "0", "command": "pushall"}});
                    tls.write_all(&publish_packet(&request, pushall.to_string().as_bytes()))?;
                    status.lock().unwrap().online = true;
                }
                3 => {
                    let tlen = u16::from_be_bytes([body[0], body[1]]) as usize;
                    let qos = (kind >> 1) & 3;
                    let start = 2 + tlen + if qos > 0 { 2 } else { 0 };
                    if let Ok(v) = serde_json::from_slice::<Value>(&body[start.min(body.len())..]) {
                        if let Some(p) = v["print"].as_object() {
                            let mut st = status.lock().unwrap();
                            for (k, val) in p {
                                st.print.insert(k.clone(), val.clone());
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if !connected && Instant::now() > deadline {
            bail!("la stampante non risponde al login");
        }
        if connected {
            while let Ok(cmd) = rx.try_recv() {
                tls.write_all(&publish_packet(&request, cmd.to_string().as_bytes()))?;
            }
            if last_ping.elapsed() > Duration::from_secs(30) {
                tls.write_all(&packet(0xC0, &[]))?;
                last_ping = Instant::now();
            }
            if last_rx.elapsed() > Duration::from_secs(90) {
                bail!("nessun dato dalla stampante da 90 s");
            }
        }
    }
}

/// Avvia (una volta) la connessione in background; None se non è configurata o il codice manca.
fn ensure(prompt: secrets::Prompt) -> Option<(Arc<Mutex<Status>>, Sender<Value>)> {
    let mut h = HANDLE.lock().unwrap();
    if let Some(h) = h.as_ref().filter(|h| h.alive.load(Ordering::SeqCst)) {
        return Some((h.status.clone(), h.tx.clone()));
    }
    if prompt == secrets::Prompt::Auto && LAST_FAIL.lock().unwrap().is_some_and(|t| t.elapsed() < Duration::from_secs(30)) {
        return None;
    }
    let started = (|| -> Result<Handle> {
        let cfg = config::load(&config::config_path())?;
        let (ip, serial) = (cfg.printer.ip.trim().to_string(), cfg.printer.serial.trim().to_string());
        if ip.is_empty() || serial.is_empty() {
            bail!("stampante non configurata (editor → Impostazioni)");
        }
        let code = secrets::get(secrets::BAMBU_CODE, prompt)?.ok_or_else(|| anyhow!("codice di accesso LAN non salvato"))?;
        let (tx, rx) = channel();
        let status = Arc::new(Mutex::new(Status::default()));
        let alive = Arc::new(AtomicBool::new(true));
        let (st, al) = (status.clone(), alive.clone());
        std::thread::spawn(move || {
            loop {
                let res = session(&ip, &serial, &code, &st, &rx);
                st.lock().unwrap().online = false;
                if let Err(e) = res {
                    eprintln!("stampante: {e:#}");
                    if format!("{e}").contains("rifiutato") {
                        break;
                    }
                }
                std::thread::sleep(Duration::from_secs(10));
            }
            al.store(false, Ordering::SeqCst);
        });
        Ok(Handle { status, tx, alive })
    })();
    match started {
        Ok(new) => {
            let out = (new.status.clone(), new.tx.clone());
            *h = Some(new);
            Some(out)
        }
        Err(e) => {
            eprintln!("stampante: {e:#}");
            *LAST_FAIL.lock().unwrap() = Some(Instant::now());
            None
        }
    }
}

/// Riparte da capo con la config nuova (es. IP o codice cambiati).
pub fn reset() {
    *HANDLE.lock().unwrap() = None;
    *LAST_FAIL.lock().unwrap() = None;
}

/// Stato attuale per i tasti; None se la stampante non è configurata.
pub fn status() -> Option<Status> {
    ensure(secrets::Prompt::Auto).map(|(s, _)| s.lock().unwrap().clone())
}

fn command(cmd: Cmd, st: &Status) -> Option<Value> {
    let print = |c: &str| json!({"print": {"sequence_id": "0", "command": c, "param": ""}});
    Some(match cmd {
        Cmd::Pause => print("pause"),
        Cmd::Resume => print("resume"),
        Cmd::Stop => print("stop"),
        Cmd::Light => json!({"system": {
            "sequence_id": "0", "command": "ledctrl", "led_node": "chamber_light",
            "led_mode": if st.light() { "off" } else { "on" },
            "led_on_time": 500, "led_off_time": 500, "loop_times": 0, "interval_time": 0
        }}),
        _ => return None,
    })
}

/// Pressione di un tasto. Stop va confermato con una seconda pressione entro 3 s.
pub fn press(cmd: Cmd) {
    if cmd == Cmd::Stop {
        let mut last = LAST_STOP.lock().unwrap();
        if !last.is_some_and(|t| t.elapsed() < Duration::from_secs(3)) {
            *last = Some(Instant::now());
            eprintln!("stampante: premi di nuovo entro 3 s per fermare la stampa");
            return;
        }
        *last = None;
    }
    std::thread::spawn(move || {
        let Some((status, tx)) = ensure(secrets::Prompt::Auto) else { return };
        let st = status.lock().unwrap().clone();
        if let Some(v) = command(cmd, &st) {
            let _ = tx.send(v);
        }
    });
}

/// Risposta a un comando di stato o di etichetta, come se fosse un comando shell.
pub fn answer(cmd: Cmd) -> Option<(bool, String)> {
    let st = status()?;
    Some(match cmd {
        Cmd::IsPrinting | Cmd::IsPaused | Cmd::IsLight => (st.is(cmd), String::new()),
        _ => (true, st.answer(cmd)),
    })
}

/// Si collega (chiedendo di aprire il portachiavi se serve) e aspetta il primo stato completo.
fn connect_and_wait() -> Result<(Status, Sender<Value>)> {
    reset();
    let (status, tx) = ensure(secrets::Prompt::Always).ok_or_else(|| anyhow!("stampante non disponibile: controlla indirizzo, numero di serie e codice"))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && !status.lock().unwrap().print.contains_key("gcode_state") {
        if HANDLE.lock().unwrap().as_ref().is_some_and(|h| !h.alive.load(Ordering::SeqCst)) {
            bail!("codice di accesso LAN rifiutato");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let st = status.lock().unwrap().clone();
    if !st.print.contains_key("gcode_state") {
        bail!("nessuna risposta dalla stampante");
    }
    Ok((st, tx))
}

/// Prova del collegamento per l'editor: stato in una riga.
pub fn check() -> Result<String> {
    let (st, _) = connect_and_wait()?;
    Ok(st.summary().lines().next().unwrap_or_default().to_string())
}

/// `cyd-deck printer ...` da terminale.
pub fn cli(args: &[String]) -> Result<()> {
    let Some(cmd) = args.first().and_then(|a| Cmd::from_arg(a)) else {
        println!("{USAGE}");
        return Ok(());
    };
    let (st, tx) = connect_and_wait()?;
    match cmd {
        Cmd::Status => println!("{}", st.summary()),
        Cmd::IsPrinting | Cmd::IsPaused | Cmd::IsLight => {
            if !st.is(cmd) {
                std::process::exit(1);
            }
        }
        Cmd::Pause | Cmd::Resume | Cmd::Stop | Cmd::Light => {
            tx.send(command(cmd, &st).expect("azione")).map_err(|_| anyhow!("connessione chiusa"))?;
            std::thread::sleep(Duration::from_millis(800));
        }
        _ => println!("{}", st.answer(cmd)),
    }
    Ok(())
}

/// Stato d'esempio per l'anteprima nell'editor.
pub fn sample() -> Status {
    let print = json!({
        "gcode_state": "RUNNING", "mc_percent": 42, "mc_remaining_time": 83, "layer_num": 63,
        "total_layer_num": 150, "nozzle_temper": 220.0, "bed_temper": 60.0, "subtask_name": "Benchy"
    });
    Status { online: true, print: serde_json::from_value(print).unwrap_or_default() }
}

/// Impronta di ciò che il widget mostra: se cambia, va ridisegnato.
pub fn widget_key(st: Option<&Status>) -> String {
    st.map_or_else(String::new, |s| {
        [Cmd::State, Cmd::Progress, Cmd::Remaining, Cmd::Layer, Cmd::Temps, Cmd::File].iter().map(|&c| s.answer(c)).collect::<Vec<_>>().join("|")
    })
}

/// Per l'editor: la stampante è configurata?
pub fn configured(cfg: &Config) -> bool {
    !cfg.printer.ip.trim().is_empty() && !cfg.printer.serial.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mqtt_packets_roundtrip() {
        let p = publish_packet("device/x/report", &vec![b'a'; 300]);
        let mut buf = p.clone();
        buf.extend([0xD0, 0]);
        let (kind, body) = take_packet(&mut buf).unwrap();
        assert_eq!(kind, 0x30);
        assert_eq!(body.len(), 2 + 15 + 300);
        assert_eq!(take_packet(&mut buf), Some((0xD0, vec![])));
        let mut partial = p[..10].to_vec();
        assert_eq!(take_packet(&mut partial), None);
    }

    #[test]
    fn parses_commands_and_status() {
        assert_eq!(Cmd::parse("cyd-deck printer pause"), Some(Cmd::Pause));
        assert_eq!(Cmd::parse("cyd-deck printer pause && x"), None);
        assert_eq!(Cmd::parse(&Cmd::IsLight.shell()), Some(Cmd::IsLight));
        let mut st = Status { online: true, ..Status::default() };
        st.print = serde_json::from_value(json!({
            "gcode_state": "RUNNING", "mc_percent": 42, "mc_remaining_time": 83,
            "layer_num": 12, "total_layer_num": 150, "nozzle_temper": 219.6, "bed_temper": 60.1,
            "lights_report": [{"node": "chamber_light", "mode": "on"}]
        }))
        .unwrap();
        assert_eq!(st.answer(Cmd::Progress), "42%");
        assert_eq!(st.answer(Cmd::Remaining), "1h 23m");
        assert_eq!(st.answer(Cmd::Layer), "12/150");
        assert_eq!(st.answer(Cmd::Temps), "220° / 60°");
        assert!(st.is(Cmd::IsPrinting) && st.is(Cmd::IsLight));
        st.print.insert("gcode_state".into(), json!("FINISH"));
        assert_eq!(st.answer(Cmd::Progress), "finita");
    }
}
