//! Protocollo seriale verso il firmware.
//!
//! Host -> scheda (binario, ogni pacchetto inizia con 0xA5):
//!   A5 01 x:u16 y:u16 w:u16 h:u16 <w*h*2 byte RGB565 BE>   blit
//!   A5 02 on:u8                                           retroilluminazione
//!   A5 03                                                 ping (risposta "HELLO ...")
//!   A5 04 madctl:u8                                       orientamento/ordine colori
//!   A5 05 on:u8                                           inversione colori
//!   A5 06 len:u16 <ssid\0password\0token\0id>          salva il Wi-Fi (solo via USB; len 0 = spento)
//!   A5 07                                                 stato della rete (risposta "NET ...")
//!   A5 08 what:u8                                         1 = riavvia la scheda, 2 = spegnila (sonno profondo)
//!   A5 09 english:u8                                      lingua dei testi della scheda (la ricorda)
//! Scheda -> host (testo, una riga per evento):
//!   HELLO cyd-deck <versione>   seguito da   B <millivolt batteria>
//!   D x y | M x y | U x y                                 tocco giù / movimento / su
//!   NET <off|connecting|up> <ip> <id>                     | OK wifi | ERR wifi
//!
//! Via Wi-Fi (TCP 47800) prima del protocollo c'è un'autenticazione reciproca:
//!   scheda: "CYD 2 <nonce_b> <id>"
//!   host:   "AUTH <sha256(token:nonce_b:host)> <nonce_h>"
//!   scheda: "OK <sha256(token:nonce_h:board)>"
//! La scheda risponde a "CYD?" in broadcast UDP sulla porta 47801 con "CYD <id> <porta>".

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, UdpSocket};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serialport::SerialPortType;
use sha2::{Digest, Sha256};

use crate::app::{Event, Phase};
use crate::config::Wifi;

pub const TCP_PORT: u16 = 47800;
const DISCOVERY_PORT: u16 = 47801;

const MAGIC: u8 = 0xA5;
/// USB-Serial-JTAG integrato nell'ESP32-S3.
const ESP_VID: u16 = 0x303a;
const ESP_PID: u16 = 0x1001;

pub fn find_port(configured: &str) -> Option<String> {
    if configured != "auto" {
        return std::path::Path::new(configured).exists().then(|| configured.to_string());
    }
    serialport::available_ports().ok()?.into_iter().find_map(|p| match p.port_type {
        SerialPortType::UsbPort(u) if u.vid == ESP_VID && u.pid == ESP_PID => Some(p.port_name),
        _ => None,
    })
}

pub fn list_ports() {
    match serialport::available_ports() {
        Ok(ports) => {
            for p in ports {
                match p.port_type {
                    SerialPortType::UsbPort(u) => println!(
                        "{}  {:04x}:{:04x}  {}",
                        p.port_name,
                        u.vid,
                        u.pid,
                        u.product.unwrap_or_default()
                    ),
                    _ => println!("{}", p.port_name),
                }
            }
        }
        Err(e) => eprintln!("{}", tr!("errore: {}", e)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    Usb,
    Wifi,
}

pub struct Device {
    port: Box<dyn Write + Send>,
    pub link: Link,
    /// Per chiudere davvero la connessione: il thread di lettura ne tiene una copia.
    tcp: Option<TcpStream>,
    /// Ultima volta che la scheda ha mandato qualcosa (la aggiorna il thread di lettura).
    last_rx: Arc<Mutex<Instant>>,
    /// Ferma il thread di lettura quando la sessione finisce: con l'USB la sua copia della porta
    /// resterebbe aperta (e bloccata) se la scheda si riavvia senza staccarsi.
    stop: Arc<AtomicBool>,
}

impl Drop for Device {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(s) = &self.tcp {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }
}

fn hex_sha(s: &str) -> String {
    Sha256::digest(s).iter().map(|b| format!("{b:02x}")).collect()
}

/// Cerca la scheda sulla rete locale: broadcast UDP più l'ultimo indirizzo noto.
pub fn discover(wifi: &Wifi) -> Option<SocketAddr> {
    if !wifi.address.is_empty() {
        let ip: IpAddr = wifi.address.parse().ok()?;
        return Some(SocketAddr::new(ip, TCP_PORT));
    }
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.set_broadcast(true).ok()?;
    sock.set_read_timeout(Some(Duration::from_millis(250))).ok()?;
    let mut targets = vec![SocketAddr::from(([255, 255, 255, 255], DISCOVERY_PORT))];
    if let Some(ip) = std::fs::read_to_string(last_ip_path()).ok().and_then(|s| s.trim().parse::<IpAddr>().ok()) {
        targets.insert(0, SocketAddr::new(ip, DISCOVERY_PORT));
    }
    // Firewall come ufw scartano le risposte a un broadcast (arrivano da un altro indirizzo):
    // ogni tanto si chiede direttamente a tutti gli indirizzi della rete locale.
    static ATTEMPT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    if ATTEMPT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 5 == 0 {
        targets.extend(local_subnet_hosts().into_iter().map(|ip| SocketAddr::new(ip.into(), DISCOVERY_PORT)));
    }
    let id = wifi.id();
    let deadline = Instant::now() + Duration::from_millis(900);
    let mut buf = [0u8; 128];
    let mut next_send = Instant::now();
    while Instant::now() < deadline {
        if Instant::now() >= next_send {
            for t in &targets {
                let _ = sock.send_to(b"CYD?", t);
            }
            next_send = Instant::now() + Duration::from_millis(300);
        }
        let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
        let text = String::from_utf8_lossy(&buf[..n]);
        let mut it = text.split_whitespace();
        // Risponde solo una scheda configurata con la nostra rete e il nostro codice.
        if it.next() == Some("CYD") && it.next() == Some(id.as_str()) {
            let port = it.next().and_then(|p| p.parse().ok()).unwrap_or(TCP_PORT);
            return Some(SocketAddr::new(from.ip(), port));
        }
    }
    None
}

/// Indirizzi della /24 di ogni interfaccia di rete attiva (esclusi loopback e container).
fn local_subnet_hosts() -> Vec<std::net::Ipv4Addr> {
    let Some(out) = crate::app::shell_output("ip -4 -o addr show up") else { return Vec::new() };
    let mut hosts = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        let (Some(dev), Some(cidr)) = (f.get(1), f.get(3)) else { continue };
        if ["lo", "docker", "br-", "veth", "virbr", "tun", "wg"].iter().any(|p| dev.starts_with(p)) {
            continue;
        }
        let Some(Ok(ip)) = cidr.split('/').next().map(str::parse::<std::net::Ipv4Addr>) else { continue };
        let [a, b, c, _] = ip.octets();
        hosts.extend((1..=254u8).map(|d| std::net::Ipv4Addr::new(a, b, c, d)));
    }
    hosts
}

fn last_ip_path() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache/cyd-deck/last-ip")
}

impl Device {
    /// Apre la porta e avvia un thread che inoltra gli eventi della scheda su `tx`.
    pub fn open(path: &str, tx: Sender<Event>) -> Result<Self> {
        let port = serialport::new(path, 115_200)
            .timeout(Duration::from_secs(2))
            .open()
            .with_context(|| format!("apertura {path}"))?;
        let mut reader = port.try_clone()?;
        reader.set_timeout(Duration::from_millis(200))?;
        let last_rx = Arc::new(Mutex::new(Instant::now()));
        let stop = Arc::new(AtomicBool::new(false));
        let (seen, stopped) = (last_rx.clone(), stop.clone());
        thread::spawn(move || read_loop(reader, tx, true, seen, stopped));
        Ok(Self { port, link: Link::Usb, tcp: None, last_rx, stop })
    }

    /// Si collega alla scheda via Wi-Fi e si autentica con il codice condiviso.
    pub fn connect(addr: SocketAddr, token: &str, tx: Sender<Event>) -> Result<Self> {
        let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3)).with_context(|| format!("connessione a {addr}"))?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(Duration::from_secs(3)))?;
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut w = stream.try_clone()?;
        let mut line = String::new();
        // Righe limitate: un finto "deck" sulla rete non può riempire la memoria.
        (&mut reader).take(256).read_line(&mut line)?;
        let mut it = line.split_whitespace();
        let (Some("CYD"), Some(_ver), Some(nb)) = (it.next(), it.next(), it.next()) else {
            bail!("{}", tr!("{} non è una scheda cyd-deck", addr));
        };
        let nh = crate::config::new_token()[..16].to_string();
        writeln!(w, "AUTH {} {nh}", hex_sha(&format!("{token}:{nb}:host")))?;
        line.clear();
        (&mut reader).take(256).read_line(&mut line).context(tr!("la scheda ha rifiutato il codice"))?;
        if line.split_whitespace().nth(1) != Some(hex_sha(&format!("{token}:{nh}:board")).as_str()) {
            bail!("{}", tr!("la scheda non ha dimostrato di conoscere il codice"));
        }
        // Da qui le letture non hanno timeout: la scheda parla solo quando c'è un tocco.
        stream.set_read_timeout(Some(Duration::from_millis(200)))?;
        let _ = std::fs::create_dir_all(last_ip_path().parent().unwrap());
        let _ = std::fs::write(last_ip_path(), addr.ip().to_string());
        let last_rx = Arc::new(Mutex::new(Instant::now()));
        let stop = Arc::new(AtomicBool::new(false));
        let (seen, stopped) = (last_rx.clone(), stop.clone());
        thread::spawn(move || read_loop(reader, tx, false, seen, stopped));
        Ok(Self { port: Box::new(w), link: Link::Wifi, tcp: Some(stream), last_rx, stop })
    }

    /// Da quanto la scheda non manda niente. Risponde a ogni ping, quindi un silenzio lungo
    /// vuol dire che non c'è più (riavviata, spenta, fuori portata), anche se il TCP non lo sa ancora.
    pub fn silent_for(&self) -> Duration {
        self.last_rx.lock().map_or(Duration::ZERO, |t| t.elapsed())
    }

    fn send(&mut self, bytes: &[u8]) -> Result<()> {
        self.port.write_all(bytes)?;
        Ok(())
    }

    pub fn query_net(&mut self) -> Result<()> {
        self.send(&[MAGIC, 0x07])
    }

    pub fn send_wifi(&mut self, payload: &[u8]) -> Result<()> {
        let mut pkt = vec![MAGIC, 0x06];
        pkt.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        pkt.extend_from_slice(payload);
        self.send(&pkt)?;
        self.port.flush()?;
        Ok(())
    }

    pub fn ping(&mut self) -> Result<()> {
        self.send(&[MAGIC, 0x03])
    }

    pub fn backlight(&mut self, on: bool) -> Result<()> {
        self.send(&[MAGIC, 0x02, on as u8])
    }

    /// Riavvia la scheda (`off = false`) o la spegne: si riaccende con un tocco o con BOOT.
    pub fn power(&mut self, off: bool) -> Result<()> {
        self.send(&[MAGIC, 0x08, if off { 2 } else { 1 }])
    }

    /// Lingua dei testi che la scheda mostra da sola (attesa, spegnimento).
    pub fn language(&mut self, english: bool) -> Result<()> {
        self.send(&[MAGIC, 0x09, english as u8])
    }

    pub fn madctl(&mut self, v: u8) -> Result<()> {
        self.send(&[MAGIC, 0x04, v])
    }

    pub fn invert(&mut self, on: bool) -> Result<()> {
        self.send(&[MAGIC, 0x05, on as u8])
    }

    pub fn blit(&mut self, x: u32, y: u32, w: u32, h: u32, pixels: &[u8]) -> Result<()> {
        debug_assert_eq!(pixels.len(), (w * h * 2) as usize);
        let mut pkt = Vec::with_capacity(10 + pixels.len());
        pkt.extend_from_slice(&[MAGIC, 0x01]);
        for v in [x, y, w, h] {
            pkt.extend_from_slice(&(v as u16).to_le_bytes());
        }
        pkt.extend_from_slice(pixels);
        self.send(&pkt)?;
        self.port.flush()?;
        Ok(())
    }

    /// Manda ping finché la scheda risponde HELLO.
    pub fn handshake(&mut self, rx: &Receiver<Event>) -> Result<()> {
        while rx.try_recv().is_ok() {}
        for _ in 0..10 {
            self.ping()?;
            let deadline = Instant::now() + Duration::from_millis(300);
            while let Some(left) = deadline.checked_duration_since(Instant::now()) {
                match rx.recv_timeout(left) {
                    Ok(Event::Hello) => return Ok(()),
                    Ok(Event::Disconnected) => bail!("{}", tr!("scheda scollegata")),
                    Ok(_) => {}
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => bail!("canale chiuso"),
                }
            }
        }
        bail!("{}", tr!("la scheda non risponde: il firmware cyd-deck è flashato?"))
    }
}

/// Valore MADCTL dell'ILI9341 per ogni rotazione (come TFT_eSPI).
pub fn madctl(rotation: u8, bgr: bool) -> u8 {
    const MX: u8 = 0x40;
    const MY: u8 = 0x80;
    const MV: u8 = 0x20;
    let v = match rotation % 4 {
        0 => MX,
        1 => MV,
        2 => MY,
        _ => MX | MY | MV,
    };
    if bgr { v | 0x08 } else { v }
}

pub fn screen_size(rotation: u8) -> (u32, u32) {
    if rotation % 2 == 1 { (320, 240) } else { (240, 320) }
}

fn read_loop(mut port: impl Read, tx: Sender<Event>, is_serial: bool, seen: Arc<Mutex<Instant>>, stop: Arc<AtomicBool>) {
    let mut buf = [0u8; 256];
    let mut line = Vec::new();
    // Le letture hanno un timeout breve, quindi il controllo arriva entro pochi decimi di secondo.
    while !stop.load(Ordering::Relaxed) {
        match port.read(&mut buf) {
            // Con la seriale 0 byte è solo un timeout; con il TCP vuol dire connessione chiusa.
            Ok(0) => {
                if !is_serial {
                    let _ = tx.send(Event::Disconnected);
                    return;
                }
            }
            Ok(n) => {
                if let Ok(mut t) = seen.lock() {
                    *t = Instant::now();
                }
                for &b in &buf[..n] {
                    if b == b'\n' {
                        if let Some(ev) = parse_line(&String::from_utf8_lossy(&line)) {
                            if tx.send(ev).is_err() {
                                return;
                            }
                        }
                        line.clear();
                    } else if line.len() < 128 {
                        line.push(b);
                    }
                }
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) => {}
            Err(_) => {
                let _ = tx.send(Event::Disconnected);
                return;
            }
        }
    }
}

fn parse_line(line: &str) -> Option<Event> {
    let mut it = line.trim().split_whitespace();
    let kind = it.next()?;
    match kind {
        "HELLO" => return Some(Event::Hello),
        "B" => return it.next()?.parse().ok().map(Event::Battery),
        "NET" => {
            let state = it.next()?.to_string();
            let ip = it.next()?.to_string();
            let id = it.next()?.to_string();
            return Some(Event::Net { state, ip, id });
        }
        "OK" | "ERR" if it.next() == Some("wifi") => return Some(Event::WifiSaved(kind == "OK")),
        _ => {}
    }
    let phase = match kind {
        "D" => Phase::Down,
        "M" => Phase::Move,
        "U" => Phase::Up,
        _ => return None,
    };
    let x = it.next()?.parse().ok()?;
    let y = it.next()?.parse().ok()?;
    Some(Event::Touch(phase, x, y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lines() {
        assert!(matches!(parse_line("B 3912"), Some(Event::Battery(3912))));
        assert!(matches!(parse_line("HELLO cyd-deck 2"), Some(Event::Hello)));
        assert!(matches!(parse_line("OK wifi"), Some(Event::WifiSaved(true))));
        assert!(matches!(parse_line("ERR wifi"), Some(Event::WifiSaved(false))));
        match parse_line("NET up 192.168.1.50 a1b2c3d4e5f6") {
            Some(Event::Net { state, ip, id }) => assert_eq!((state.as_str(), ip.as_str(), id.as_str()), ("up", "192.168.1.50", "a1b2c3d4e5f6")),
            _ => panic!("NET non riconosciuto"),
        }
        assert!(matches!(parse_line("D 10 20"), Some(Event::Touch(Phase::Down, 10, 20))));
    }

    #[test]
    fn wifi_payload() {
        let mut w = Wifi { ssid: "Casa".into(), password: "segreta".into(), token: String::new(), address: String::new(), locked: false, fingerprint: String::new() };
        assert_eq!((w.payload(), w.id()), (Vec::new(), "-".to_string()), "senza codice il Wi-Fi resta spento");
        w.token = "abc".into();
        let p = String::from_utf8(w.payload()).unwrap();
        let parts: Vec<&str> = p.split('\0').collect();
        assert_eq!(parts, ["Casa", "segreta", "abc", w.id().as_str()]);
        assert_eq!(w.id().len(), 12);
        assert!(w.payload().len() <= 256);
    }

    /// Righe a caso (come da una scheda guasta o da un impostore): nessun panic.
    #[test]
    fn garbage_lines_never_panic() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let words = ["T", "D", "U", "M", "NET", "up", "OK", "ERR", "wifi", "BAT", "HELLO", "-1", "65536", "99999999999", "", " ", "é", "\0", "0", "320", "240"];
        for _ in 0..20000 {
            let n = rnd() % 6;
            let line: Vec<&str> = (0..n).map(|_| words[(rnd() % words.len() as u64) as usize]).collect();
            let _ = parse_line(&line.join(" "));
            let bytes: Vec<u8> = (0..rnd() % 40).map(|_| rnd() as u8).collect();
            let _ = parse_line(&String::from_utf8_lossy(&bytes));
        }
    }
}
