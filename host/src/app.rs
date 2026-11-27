use std::collections::HashMap;
use std::io::BufRead;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Result, bail};

use crate::audio::{self, Channel};
use crate::bar;
use crate::claude;
use crate::config::{self, Config, Key, MixerConfig, PageKind, Widget};
use crate::device::{self, Device, Link};
use crate::macros;
use crate::media::{self, NowPlaying};
use crate::mixer::{self, Button, Hit, Layout, Rect, View};
use crate::render::{self, Canvas, Fonts, KeyVisual, Rgb, Style};
use crate::theme::{self, Theme};
use crate::widgets;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Down,
    Move,
    Up,
}

pub enum Event {
    Hello,
    Disconnected,
    /// Coordinate grezze del controller touch.
    Touch(Phase, u16, u16),
    Status { generation: u64, page: usize, idx: usize, active: Option<bool>, label: Option<String> },
    /// Azione interna richiesta da una macro in esecuzione.
    Action(String),
    /// Volumi e brano letti in background per la pagina audio.
    Mixer { generation: u64, channels: Vec<Channel>, np: Option<NowPlaying>, identity: Option<Option<String>> },
    /// Copertina pronta (vuota se non è stato possibile caricarla).
    Art { url: String, px: Option<Vec<Rgb>> },
    /// Tensione della batteria della scheda in millivolt.
    Battery(u16),
    /// Stato del Wi-Fi della scheda e impronta della rete salvata.
    Net { state: String, ip: String, id: String },
    WifiSaved(bool),
    /// Classe della finestra in primo piano in Hyprland.
    ActiveWindow(String),
}

/// Quanto scende la tensione sotto il carico della scheda (Wi-Fi e schermo).
const LOAD_MV: u16 = 45;
/// Quanto sale la tensione mentre il caricatore spinge corrente nella batteria.
const CHARGE_MV: u16 = 90;
/// Un salto così fra due letture vuol dire caricatore attaccato o staccato.
const PLUG_STEP_MV: u16 = 50;

/// Livello di una batteria LiPo dalla tensione (curva a tratti, a riposo).
pub fn battery_percent(mv: u16) -> u8 {
    const CURVE: [(u16, u8); 9] =
        [(3300, 0), (3500, 5), (3600, 12), (3700, 30), (3750, 42), (3800, 52), (3900, 66), (4000, 80), (4150, 100)];
    if mv <= CURVE[0].0 {
        return 0;
    }
    for w in CURVE.windows(2) {
        let ((v0, p0), (v1, p1)) = (w[0], w[1]);
        if mv <= v1 {
            return p0 + ((mv - v0) as u32 * (p1 - p0) as u32 / (v1 - v0) as u32) as u8;
        }
    }
    100
}

/// Icona Material per un livello di batteria.
pub fn battery_icon(percent: Option<u8>, charging: bool) -> &'static str {
    match (percent, charging) {
        (None, _) => "battery_unknown",
        (Some(_), true) => "battery_charging_full",
        (Some(p), false) => match p {
            0..=7 => "battery_alert",
            8..=20 => "battery_1_bar",
            21..=35 => "battery_2_bar",
            36..=50 => "battery_3_bar",
            51..=65 => "battery_4_bar",
            66..=80 => "battery_5_bar",
            81..=94 => "battery_6_bar",
            _ => "battery_full",
        },
    }
}

/// Indicatore della batteria del deck: c'è solo nella beta (feature "battery").
pub const BATTERY: bool = cfg!(feature = "battery");

/// Sostituisce `{battery}` in etichette e icone dei tasti.
pub fn expand(text: &str, percent: Option<u8>, charging: bool, icon: bool) -> String {
    if !text.contains("{battery}") {
        return text.to_string();
    }
    if !BATTERY {
        return if icon { String::new() } else { text.replace("{battery}", "") };
    }
    if icon {
        return battery_icon(percent, charging).to_string();
    }
    let value = percent.map_or("--".to_string(), |p| format!("{p}%"));
    text.replace("{battery}", &value)
}

/// Stato della pagina audio.
struct MixerState {
    channels: Vec<Channel>,
    np: Option<NowPlaying>,
    /// Player attivo secondo Caelestia, riletto ogni pochi secondi.
    identity: Option<String>,
    identity_at: Option<Instant>,
    art_url: String,
    art: Option<Vec<Rgb>>,
    scroll: usize,
    pressed: Option<Hit>,
    dragging: Option<usize>,
    /// Volumi/muti impostati da poco: valgono più dei dati letti, che arrivano in ritardo.
    held: HashMap<String, (f32, bool, Instant)>,
    last_set: Option<Instant>,
    pending: Option<usize>,
    busy: Arc<AtomicBool>,
    next_poll: Instant,
    /// Ultimo invio della sola barra di avanzamento.
    progress_at: Option<Instant>,
}

impl Default for MixerState {
    fn default() -> Self {
        Self {
            channels: Vec::new(),
            np: None,
            identity: None,
            identity_at: None,
            art_url: String::new(),
            art: None,
            scroll: 0,
            pressed: None,
            dragging: None,
            held: HashMap::new(),
            last_set: None,
            pending: None,
            busy: Arc::new(AtomicBool::new(false)),
            next_poll: Instant::now(),
            progress_at: None,
        }
    }
}

enum Part {
    All,
    Card,
    /// Solo la barra di avanzamento (la scheda viene comunque ridisegnata sul canvas).
    Progress,
    Rows,
    Row(usize),
}

const HOLD: Duration = Duration::from_millis(1500);
const SET_EVERY: Duration = Duration::from_millis(50);

#[derive(Default, Clone, PartialEq)]
struct KeyState {
    active: bool,
    label: Option<String>,
}

/// Lato, origine e passo della griglia dei tasti.
#[derive(Clone, Copy)]
pub struct Grid {
    pub size: u32,
    pub x0: u32,
    pub y0: u32,
    pub step: u32,
    pub cols: u32,
    pub rows: u32,
}

impl Grid {
    pub fn new(cfg: &Config, w: u32, h: u32) -> Self {
        let l = &cfg.layout;
        let (cols, rows, gap) = (l.cols.max(1), l.rows.max(1), l.gap);
        // Sotto la barra di stato, se c'è.
        let top = config::bar_height(cfg);
        let h = h - top;
        let size = ((w.saturating_sub(gap * (cols + 1))) / cols).min((h.saturating_sub(gap * (rows + 1))) / rows);
        let x0 = (w - (cols * size + (cols - 1) * gap)) / 2;
        let y0 = top + (h - (rows * size + (rows - 1) * gap)) / 2;
        Self { size, x0, y0, step: size + gap, cols, rows }
    }

    pub fn slots(&self) -> usize {
        (self.cols * self.rows) as usize
    }

    pub fn rect(&self, i: usize) -> (u32, u32) {
        let i = i as u32;
        (self.x0 + (i % self.cols) * self.step, self.y0 + (i / self.cols) * self.step)
    }

    /// Rettangolo di `w`×`h` posti a partire dal posto `i`.
    pub fn area(&self, i: usize, w: u32, h: u32) -> Rect {
        let (x, y) = self.rect(i);
        Rect { x, y, w: w * self.step - (self.step - self.size), h: h * self.step - (self.step - self.size) }
    }

    pub fn hit(&self, x: i32, y: i32) -> Option<usize> {
        (0..self.slots()).find(|&i| {
            let (kx, ky) = self.rect(i);
            let s = self.size as i32;
            x >= kx as i32 && x < kx as i32 + s && y >= ky as i32 && y < ky as i32 + s
        })
    }
}

/// Converte le coordinate grezze del touch in coordinate schermo.
pub fn map_touch(cfg: &Config, rx: u16, ry: u16) -> (i32, i32) {
    let (w, h) = device::screen_size(cfg.device.rotation);
    let t = &cfg.touch;
    let (a, b) = if t.swap_xy { (ry as i32, rx as i32) } else { (rx as i32, ry as i32) };
    let x = if t.invert_x { w as i32 - 1 - a } else { a };
    let y = if t.invert_y { h as i32 - 1 - b } else { b };
    (x, y)
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

pub(crate) fn spawn_shell(cmd: &str) {
    if let Some(c) = crate::ha::Cmd::parse(cmd) {
        crate::ha::spawn(c);
        return;
    }
    if let Some(c) = crate::printer::Cmd::parse(cmd) {
        crate::printer::press(c);
        return;
    }
    let child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
    match child {
        // Il wait evita processi zombie.
        Ok(mut c) => drop(thread::spawn(move || c.wait())),
        Err(e) => eprintln!("esecuzione fallita '{cmd}': {e}"),
    }
}

pub(crate) fn shell_output(cmd: &str) -> Option<std::process::Output> {
    Command::new("sh").arg("-c").arg(cmd).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()
}

struct Touch {
    start: (i32, i32),
    key: Option<usize>,
    swiping: bool,
    /// Il tocco ha solo riacceso lo schermo: non attiva nulla.
    wake_only: bool,
}

pub struct App {
    cfg: Config,
    cfg_path: PathBuf,
    cfg_mtime: Option<SystemTime>,
    scheme_path: PathBuf,
    scheme_mtime: Option<SystemTime>,
    theme: Theme,
    fonts: Fonts,
    canvas: Canvas,
    grid: Grid,
    page: usize,
    states: Vec<Vec<KeyState>>,
    touch: Option<Touch>,
    backlight: bool,
    last_activity: Instant,
    generation: u64,
    poll_busy: Arc<AtomicBool>,
    next_poll: Instant,
    next_file_check: Instant,
    mixer: MixerState,
    /// Ultima tensione letta dalla scheda (0 = sconosciuta o senza batteria).
    battery_mv: u16,
    /// Caricatore attaccato, dedotto dai salti di tensione (via Wi-Fi non c'è altro modo).
    charging: bool,
    link: Link,
    next_ping: Instant,
    /// Ora mostrata nella barra e minuto a cui si riferisce.
    clock: String,
    date: String,
    /// Limiti d'uso di Claude salvati dalla status line di Claude Code.
    claude: Option<claude::Limits>,
    /// Ciò che mostrano i widget della stampante, per ridisegnarli solo quando cambia.
    printer_seen: String,
    claude_mtime: Option<SystemTime>,
    clock_minute: u64,
    /// Pagina da cui si è arrivati con un cambio automatico: ci si torna lasciando l'app.
    auto_from: Option<usize>,
    /// Cambio di finestra arrivato durante un tocco, applicato al rilascio.
    pending_window: Option<String>,
    tx: Sender<Event>,
}

impl App {
    fn new(cfg_path: &Path, tx: Sender<Event>) -> Result<Self> {
        let cfg = config::load(cfg_path)?;
        let fonts = Fonts::load(&cfg.layout.icon_font, &cfg.layout.text_font)?;
        let scheme_path = config::scheme_path();
        let mut app = App {
            cfg_mtime: mtime(cfg_path),
            cfg_path: cfg_path.to_path_buf(),
            scheme_mtime: mtime(&scheme_path),
            theme: theme::for_config(&cfg.theme, &scheme_path),
            scheme_path,
            fonts,
            canvas: Canvas::new(1, 1),
            grid: Grid::new(&cfg, 320, 240),
            page: 0,
            states: Vec::new(),
            touch: None,
            backlight: true,
            last_activity: Instant::now(),
            generation: 0,
            poll_busy: Arc::new(AtomicBool::new(false)),
            next_poll: Instant::now(),
            next_file_check: Instant::now(),
            mixer: MixerState::default(),
            battery_mv: 0,
            // Riparte dallo stato scritto dal demone precedente: il salto di tensione non si rivede.
            charging: std::env::var_os("XDG_RUNTIME_DIR")
                .and_then(|d| std::fs::read_to_string(std::path::Path::new(&d).join("cyd-deck/battery")).ok())
                .is_some_and(|t| t.contains("charging")),
            link: Link::Usb,
            next_ping: Instant::now(),
            clock: bar::clock(),
            date: bar::date(),
            claude: claude::load(),
            printer_seen: String::new(),
            claude_mtime: mtime(&claude::path()),
            clock_minute: now_minute(),
            auto_from: None,
            pending_window: None,
            tx,
            cfg,
        };
        app.reset_states();
        app.check_icons();
        Ok(app)
    }
}

/// Salva ogni pagina come immagine PPM, utile per provare la config senza scheda.
pub fn preview(cfg_path: &Path, dir: &Path) -> Result<()> {
    let (tx, _rx) = mpsc::channel();
    let mut app = App::new(cfg_path, tx)?;
    std::fs::create_dir_all(dir)?;
    for page in 0..app.cfg.pages.len().max(1) {
        app.page = page;
        if let Some(cfg) = app.mixer_cfg().cloned() {
            app.mixer.channels = audio::snapshot(&cfg);
            app.mixer.np = if cfg.player { media::query(media::caelestia_player().as_deref()) } else { None };
            app.mixer.art = app.mixer.np.as_ref().and_then(|n| media::load_art(&n.art_url, mixer::ART));
        }
        app.render_all();
        let (w, h) = (app.canvas.w, app.canvas.h);
        let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
        for px in app.canvas.rgb565(0, 0, w, h).chunks(2) {
            let v = u16::from_be_bytes([px[0], px[1]]);
            let (r, g, b) = ((v >> 11) & 31, (v >> 5) & 63, v & 31);
            out.extend_from_slice(&[(r * 255 / 31) as u8, (g * 255 / 63) as u8, (b * 255 / 31) as u8]);
        }
        let path = dir.join(format!("page{}.ppm", page + 1));
        std::fs::write(&path, out)?;
        println!("{}", path.display());
    }
    Ok(())
}

fn now_minute() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs() / 60).unwrap_or(0)
}

/// Socket degli eventi di Hyprland: quello della sessione, o il più recente se Hyprland è ripartito.
fn hypr_socket() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?).join("hypr");
    if let Some(sig) = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE") {
        let p = dir.join(sig).join(".socket2.sock");
        if p.exists() {
            return Some(p);
        }
    }
    std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|e| e.path().join(".socket2.sock"))
        .filter(|p| p.exists())
        .max_by_key(|p| mtime(p))
}

/// Inoltra le finestre che vanno in primo piano ("activewindow>>classe,titolo").
fn spawn_hypr_listener(tx: Sender<Event>) {
    thread::spawn(move || {
        loop {
            if let Some(stream) = hypr_socket().and_then(|p| std::os::unix::net::UnixStream::connect(p).ok()) {
                for line in std::io::BufReader::new(stream).lines() {
                    let Ok(line) = line else { break };
                    if let Some(rest) = line.strip_prefix("activewindow>>") {
                        let class = rest.split(',').next().unwrap_or_default().to_string();
                        if tx.send(Event::ActiveWindow(class)).is_err() {
                            return;
                        }
                    }
                }
            }
            thread::sleep(Duration::from_secs(5));
        }
    });
}

fn active_class() -> Option<String> {
    let out = shell_output("hyprctl activewindow -j")?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    v["class"].as_str().map(str::to_string)
}

/// Ogni quanto si manda un ping alla scheda.
const PING_EVERY: Duration = Duration::from_secs(4);
/// Dopo quanto silenzio la scheda è considerata sparita.
const BOARD_SILENT: Duration = Duration::from_secs(10);

pub fn run(cfg_path: &Path) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    let mut app = App::new(cfg_path, tx.clone())?;
    spawn_hypr_listener(tx.clone());

    let mut waiting_logged = false;
    let mut last_error = String::new();
    loop {
        // L'USB ha la precedenza: è più veloce e serve per inviare il Wi-Fi alla scheda.
        let dev = match device::find_port(&app.cfg.device.port) {
            Some(path) => Some(Device::open(&path, tx.clone()).map(|d| (d, path))),
            None if app.cfg.wifi.enabled() => device::discover(&app.cfg.wifi)
                .map(|addr| Device::connect(addr, &app.cfg.wifi.token, tx.clone()).map(|d| (d, format!("{addr} (Wi-Fi)")))),
            None => None,
        };
        match dev {
            Some(Ok((dev, name))) => {
                waiting_logged = false;
                last_error.clear();
                eprintln!("{}", tr!("connesso a {}", name));
                if let Err(e) = app.session(dev, &rx) {
                    eprintln!("{}", tr!("sessione terminata: {}", format!("{e:#}")));
                } else {
                    eprintln!("{}", tr!("scheda scollegata"));
                }
            }
            Some(Err(e)) => {
                let msg = format!("{e:#}");
                if msg != last_error {
                    eprintln!("{msg}");
                    last_error = msg;
                }
            }
            None if !waiting_logged => {
                if app.cfg.wifi.enabled() {
                    eprintln!("{}", tr!("in attesa della scheda (USB o Wi-Fi)..."));
                } else {
                    eprintln!("{}", tr!("in attesa della scheda..."));
                }
                waiting_logged = true;
            }
            None => {}
        }
        thread::sleep(Duration::from_secs(2));
        // Mentre la scheda è assente teniamo comunque aggiornata la config.
        app.check_files();
    }
}

impl App {
    fn page_keys(&self) -> &[Key] {
        self.cfg.pages.get(self.page).map(|p| p.keys.as_slice()).unwrap_or(&[])
    }

    fn reset_states(&mut self) {
        self.generation += 1;
        self.states = self.cfg.pages.iter().map(|p| vec![KeyState::default(); p.keys.len()]).collect();
        self.page = self.page.min(self.cfg.pages.len().saturating_sub(1));
        // Una pagina nascosta resta aperta solo se ci si è arrivati con la sua app.
        if self.auto_from.is_none() && self.cfg.pages.get(self.page).is_some_and(|p| p.hidden) {
            self.page = config::step_page(&self.cfg.pages, self.page, 1);
        }
    }

    fn check_icons(&self) {
        for p in &self.cfg.pages {
            for k in &p.keys {
                for icon in [&k.icon, &k.icon_active].into_iter().flatten().filter(|i| !i.contains('{')) {
                    if !self.fonts.has_icon(icon) {
                        eprintln!("{}", tr!("attenzione: icona '{}' (pagina '{}') non trovata nel font", icon, p.name));
                    }
                }
            }
        }
    }

    /// Via USB confronta il Wi-Fi salvato sulla scheda con la config e, se serve, lo aggiorna.
    /// Restituisce true se la scheda si sta riavviando con la nuova rete.
    fn sync_wifi(&mut self, dev: &mut Device, rx: &Receiver<Event>) -> Result<bool> {
        let wifi = &self.cfg.wifi;
        if !wifi.ssid.is_empty() && wifi.token.is_empty() {
            eprintln!("{}", tr!("wifi: manca il codice di abbinamento, apri l'editor (Impostazioni → Wi-Fi) per generarlo"));
            return Ok(false);
        }
        dev.query_net()?;
        let deadline = Instant::now() + Duration::from_millis(800);
        let mut board_id = None;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(Event::Net { state, ip, id }) => {
                    if state == "up" {
                        eprintln!("{}", tr!("wifi della scheda: connesso, {}", ip));
                    }
                    board_id = Some(id);
                    break;
                }
                Ok(Event::Battery(mv)) => self.battery_mv = mv,
                Ok(Event::Disconnected) => bail!("{}", tr!("scheda scollegata")),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        // Un firmware vecchio, senza Wi-Fi, non risponde: si lascia stare.
        let Some(board_id) = board_id else { return Ok(false) };
        if board_id == wifi.id() {
            return Ok(false);
        }
        // La scheda va aggiornata (per esempio dopo un flash che l'ha cancellata), ma con il
        // portachiavi chiuso la password non si conosce: solo ora si chiede di sbloccarlo.
        if wifi.locked {
            match crate::secrets::get(crate::secrets::WIFI_PASSWORD, crate::secrets::Prompt::Auto) {
                Ok(p) => {
                    self.cfg.wifi.password = p.unwrap_or_default();
                    self.cfg.wifi.locked = false;
                }
                Err(_) => {
                    eprintln!("{}", tr!("wifi: la scheda va aggiornata ma il portachiavi cyd-deck è chiuso: sbloccalo (editor → Impostazioni) e ricollegala via USB"));
                    return Ok(false);
                }
            }
        }
        let wifi = &self.cfg.wifi;
        if board_id == wifi.id() {
            return Ok(false);
        }
        dev.send_wifi(&wifi.payload())?;
        let deadline = Instant::now() + Duration::from_secs(3);
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(Event::WifiSaved(true)) => {
                    if wifi.enabled() {
                        eprintln!("{}", tr!("wifi: rete '{}' salvata sulla scheda, che si riavvia", wifi.ssid));
                    } else {
                        eprintln!("{}", tr!("wifi: disattivato sulla scheda, che si riavvia"));
                    }
                    return Ok(true);
                }
                Ok(Event::WifiSaved(false)) => bail!("{}", tr!("la scheda non è riuscita a salvare il Wi-Fi")),
                Ok(Event::Disconnected) => return Ok(true),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        bail!("{}", tr!("la scheda non ha confermato il salvataggio del Wi-Fi"))
    }

    fn session(&mut self, mut dev: Device, rx: &Receiver<Event>) -> Result<()> {
        dev.handshake(rx)?;
        self.link = dev.link;
        if dev.link == Link::Usb && self.sync_wifi(&mut dev, rx)? {
            return Ok(());
        }
        // Il primo ping parte subito: la risposta porta il livello della batteria.
        self.next_ping = Instant::now();
        let d = &self.cfg.device;
        dev.madctl(device::madctl(d.rotation, d.bgr))?;
        dev.invert(d.invert)?;
        dev.language(crate::i18n::english())?;
        dev.backlight(true)?;
        self.backlight = true;
        self.last_activity = Instant::now();
        self.touch = None;
        self.next_poll = Instant::now();
        self.full_redraw(&mut dev)?;
        // La finestra già in primo piano può volere la sua pagina.
        if let Some(class) = active_class() {
            self.on_active_window(&mut dev, &class)?;
        }

        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Event::Disconnected) => return Ok(()),
                Ok(Event::Hello) => {}
                Ok(Event::Action(a)) => self.run_action(&mut dev, &a)?,
                Ok(Event::Touch(phase, x, y)) => self.on_touch(&mut dev, phase, x, y)?,
                Ok(Event::Status { generation, page, idx, active, label }) => {
                    if generation != self.generation {
                        continue;
                    }
                    let Some(st) = self.states.get_mut(page).and_then(|s| s.get_mut(idx)) else { continue };
                    let new = KeyState { active: active.unwrap_or(false), label };
                    if *st != new {
                        *st = new;
                        let pressed = self.touch.as_ref().is_some_and(|t| t.key == Some(idx) && !t.swiping);
                        if page == self.page && !pressed {
                            self.draw_key(&mut dev, idx, false)?;
                        }
                    }
                }
                Ok(Event::Mixer { generation, channels, np, identity }) => {
                    self.mixer.busy.store(false, Ordering::SeqCst);
                    if let Some(id) = identity {
                        self.mixer.identity = id;
                        self.mixer.identity_at = Some(Instant::now());
                    }
                    if generation == self.generation {
                        self.on_mixer_data(&mut dev, channels, np)?;
                    }
                }
                Ok(Event::Battery(mv)) => self.on_battery(&mut dev, mv)?,
                Ok(Event::Net { .. } | Event::WifiSaved(_)) => {}
                Ok(Event::ActiveWindow(class)) => self.on_active_window(&mut dev, &class)?,
                Ok(Event::Art { url, px }) => {
                    if url == self.mixer.art_url {
                        self.mixer.art = px;
                        self.mixer_blit(&mut dev, Part::Card)?;
                        for i in self.player_widgets() {
                            self.widget_blit(&mut dev, i, false)?;
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
            if self.touch.is_none() {
                if let Some(class) = self.pending_window.take() {
                    self.on_active_window(&mut dev, &class)?;
                }
            }
            self.tick(&mut dev)?;
        }
    }

    fn tick(&mut self, dev: &mut Device) -> Result<()> {
        let now = Instant::now();
        if now >= self.next_file_check {
            let (cfg_changed, scheme_changed) = self.check_files();
            if cfg_changed {
                let d = &self.cfg.device;
                dev.madctl(device::madctl(d.rotation, d.bgr))?;
                dev.invert(d.invert)?;
                dev.language(crate::i18n::english())?;
            }
            if cfg_changed || scheme_changed {
                self.full_redraw(dev)?;
            } else if self.check_claude() {
                for i in self.widgets_of(&[Widget::ClaudeLimits]) {
                    self.widget_blit(dev, i, false)?;
                }
            }
        }
        if now >= self.next_poll {
            self.next_poll = now + Duration::from_secs_f32(self.cfg.layout.poll_interval.max(0.2));
            self.spawn_poll();
            let printers = self.widgets_of(&[Widget::Printer]);
            if !printers.is_empty() {
                let seen = crate::printer::widget_key(crate::printer::status().as_ref());
                if seen != self.printer_seen {
                    self.printer_seen = seen;
                    for i in printers {
                        self.widget_blit(dev, i, false)?;
                    }
                }
            }
        }
        if now_minute() != self.clock_minute {
            self.clock_minute = now_minute();
            self.clock = bar::clock();
            self.date = bar::date();
            if self.cfg.bar.show && self.cfg.bar.clock {
                self.bar_blit(dev)?;
            }
            // Orologi e tempo all'azzeramento dei limiti di Claude cambiano ogni minuto.
            for i in self.widgets_of(&[Widget::Clock, Widget::ClaudeLimits]) {
                self.widget_blit(dev, i, false)?;
            }
        }
        // Il ping tiene viva la connessione e porta con sé il livello della batteria.
        if now >= self.next_ping {
            self.next_ping = now + PING_EVERY;
            dev.ping()?;
        }
        // Nessuna risposta a due ping di fila: la scheda non c'è più, la si cerca di nuovo subito
        // invece di aspettare che il TCP se ne accorga (anche molti minuti).
        if dev.silent_for() > BOARD_SILENT {
            bail!("{}", tr!("la scheda non risponde più"));
        }
        if self.backlight && now >= self.mixer.next_poll && self.poll_cfg().is_some() {
            self.mixer.next_poll = now + Duration::from_secs(1);
            self.spawn_mixer_poll();
        }
        if let Some(i) = self.mixer.pending {
            if self.mixer.last_set.is_none_or(|t| t.elapsed() >= SET_EVERY) {
                self.send_volume(i);
            }
        }
        let idle = self.cfg.device.idle_timeout;
        if idle > 0 && self.backlight && self.touch.is_none() && self.last_activity.elapsed() > Duration::from_secs(idle) {
            dev.backlight(false)?;
            self.backlight = false;
        }
        Ok(())
    }

    /// Ricarica config e schema colori se i file sono cambiati.
    fn check_files(&mut self) -> (bool, bool) {
        self.next_file_check = Instant::now() + Duration::from_secs(1);
        let mut cfg_changed = false;
        let m = mtime(&self.cfg_path);
        if m != self.cfg_mtime {
            self.cfg_mtime = m;
            match config::load(&self.cfg_path) {
                Ok(cfg) => {
                    let fonts_changed = cfg.layout.icon_font != self.cfg.layout.icon_font
                        || cfg.layout.text_font != self.cfg.layout.text_font;
                    self.cfg = cfg;
                    crate::ha::reset();
                    crate::printer::reset();
                    if fonts_changed {
                        match Fonts::load(&self.cfg.layout.icon_font, &self.cfg.layout.text_font) {
                            Ok(f) => self.fonts = f,
                            Err(e) => eprintln!("font: {e:#}"),
                        }
                    }
                    self.reset_states();
                    self.check_icons();
                    self.next_poll = Instant::now();
                    cfg_changed = true;
                    eprintln!("{}", tr!("config ricaricata"));
                }
                Err(e) => eprintln!("{}", tr!("config non valida, tengo la precedente: {}", format!("{e:#}"))),
            }
        }
        let m = mtime(&self.scheme_path);
        let scheme_changed = m != self.scheme_mtime;
        if scheme_changed {
            self.scheme_mtime = m;
        }
        if scheme_changed || cfg_changed {
            crate::i18n::set(&self.cfg.language);
            self.theme = theme::for_config(&self.cfg.theme, &self.scheme_path);
        }
        (cfg_changed, scheme_changed)
    }

    fn spawn_poll(&self) {
        let jobs: Vec<(usize, Option<String>, Option<String>)> = self
            .page_keys()
            .iter()
            .enumerate()
            .filter(|(_, k)| k.state.is_some() || k.label_cmd.is_some())
            .map(|(i, k)| (i, k.state.clone(), k.label_cmd.clone()))
            .collect();
        if jobs.is_empty() || self.poll_busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let (tx, busy, generation, page) = (self.tx.clone(), self.poll_busy.clone(), self.generation, self.page);
        thread::spawn(move || {
            // I comandi per Home Assistant si rispondono con una sola lettura degli stati.
            let mut ha = crate::ha::Snapshot::default();
            let mut run = |c: &str| {
                if let Some(cmd) = crate::ha::Cmd::parse(c) {
                    return ha.answer(&cmd);
                }
                if let Some(cmd) = crate::printer::Cmd::parse(c) {
                    return crate::printer::answer(cmd);
                }
                shell_output(c).map(|o| (o.status.success(), String::from_utf8_lossy(&o.stdout).trim().to_string()))
            };
            for (idx, state, label_cmd) in jobs {
                let active = state.map(|c| run(&c).is_some_and(|(ok, _)| ok));
                let label = label_cmd.and_then(|c| run(&c)).map(|(_, out)| out).filter(|s| !s.is_empty());
                if tx.send(Event::Status { generation, page, idx, active, label }).is_err() {
                    break;
                }
            }
            busy.store(false, Ordering::SeqCst);
        });
    }

    fn render_all(&mut self) {
        let (w, h) = device::screen_size(self.cfg.device.rotation);
        self.grid = Grid::new(&self.cfg, w, h);
        self.canvas = Canvas::new(w, h);
        if self.mixer_render(Part::All).is_none() {
            self.canvas.fill(self.theme.background);
            let owners = self.owners();
            for i in (0..self.grid.slots()).filter(|&i| owners[i] == i) {
                self.render_key(i, false);
            }
            self.render_dots();
        }
        self.render_bar();
    }

    fn render_bar(&mut self) {
        let (pct, charging) = self.battery();
        let page = self.cfg.pages.get(self.page).map(|p| p.name.as_str()).unwrap_or_default();
        let info = bar::Info { page, clock: &self.clock, battery: pct, charging, link: Some(self.link) };
        bar::draw(&mut self.canvas, &self.fonts, &self.theme, &self.cfg.bar, config::bar_height(&self.cfg), &info);
    }

    fn bar_blit(&mut self, dev: &mut Device) -> Result<()> {
        let h = config::bar_height(&self.cfg);
        if h == 0 {
            return Ok(());
        }
        self.render_bar();
        let w = self.canvas.w;
        dev.blit(0, 0, w, h, &self.canvas.rgb565(0, 0, w, h))
    }

    /// Porta il deck sulla pagina dell'app in primo piano, e indietro quando la si lascia.
    fn on_active_window(&mut self, dev: &mut Device, class: &str) -> Result<()> {
        // L'editor del deck stesso non conta: si può modificare senza che la pagina scappi.
        if class == "cyd-deck" {
            return Ok(());
        }
        if self.touch.is_some() {
            self.pending_window = Some(class.to_string());
            return Ok(());
        }
        match self.cfg.pages.iter().position(|p| p.matches_app(class)) {
            Some(p) => {
                self.auto_from.get_or_insert(self.page);
                self.set_page(dev, p)
            }
            None => match self.auto_from.take() {
                Some(back) => self.set_page(dev, back.min(self.cfg.pages.len().saturating_sub(1))),
                None => Ok(()),
            },
        }
    }

    fn full_redraw(&mut self, dev: &mut Device) -> Result<()> {
        self.render_all();
        let (w, h) = (self.canvas.w, self.canvas.h);
        dev.blit(0, 0, w, h, &self.canvas.rgb565(0, 0, w, h))
    }

    fn render_dots(&mut self) {
        // Solo le pagine raggiungibili con lo swipe.
        let pages = config::visible_pages(&self.cfg.pages);
        let n = pages.len();
        if n < 2 {
            return;
        }
        let g = self.grid;
        let bottom = g.y0 + g.rows * g.step - (g.step - g.size);
        let cy = (bottom + self.canvas.h) as f32 / 2.0;
        let spacing = 10.0;
        let x0 = self.canvas.w as f32 / 2.0 - spacing * (n - 1) as f32 / 2.0;
        for i in 0..n {
            let col = if pages[i] == self.page { self.theme.primary } else { self.theme.outline_variant };
            self.canvas.circle(x0 + spacing * i as f32, cy, 2.5, col);
        }
    }

    fn render_key(&mut self, i: usize, pressed: bool) {
        let (x, y) = self.grid.rect(i);
        let keys = self.cfg.pages.get(self.page).map(|p| p.keys.as_slice()).unwrap_or(&[]);
        let Some(key) = keys.get(i).filter(|k| !k.is_empty()) else {
            self.canvas.fill_rect(x, y, self.grid.size, self.grid.size, self.theme.background);
            return;
        };
        if key.widget.is_some() {
            return self.render_widget(i);
        }
        let st = self.states.get(self.page).and_then(|s| s.get(i)).cloned().unwrap_or_default();
        let style = if pressed {
            Style::Pressed
        } else if st.active {
            Style::Active
        } else {
            Style::Normal
        };
        let icon = if st.active { key.icon_active.as_ref().or(key.icon.as_ref()) } else { key.icon.as_ref() };
        let label = st.label.as_ref().or(key.label.as_ref());
        let (pct, charging) = self.battery();
        let icon = icon.map(|i| expand(i, pct, charging, true));
        let label = label.map(|l| expand(l, pct, charging, false));
        let visual = KeyVisual { icon: icon.as_deref(), label: label.as_deref(), style };
        render::draw_key(&mut self.canvas, &self.fonts, &self.theme, x, y, self.grid.size, self.cfg.layout.radius, &visual);
    }

    fn draw_key(&mut self, dev: &mut Device, i: usize, pressed: bool) -> Result<()> {
        // I tasti coperti da una parte audio grande non si vedono.
        if i >= self.grid.slots() || self.owners()[i] != i {
            return Ok(());
        }
        self.render_key(i, pressed);
        let r = self.slot_area(i);
        dev.blit(r.x, r.y, r.w, r.h, &self.canvas.rgb565(r.x, r.y, r.w, r.h))
    }

    fn on_touch(&mut self, dev: &mut Device, phase: Phase, rx: u16, ry: u16) -> Result<()> {
        let (x, y) = map_touch(&self.cfg, rx, ry);
        self.last_activity = Instant::now();
        match phase {
            Phase::Down => {
                if !self.backlight {
                    dev.backlight(true)?;
                    self.backlight = true;
                    self.touch = Some(Touch { start: (x, y), key: None, swiping: false, wake_only: true });
                    // I dati del mixer non sono stati aggiornati a schermo spento.
                    self.mixer.next_poll = Instant::now();
                    return Ok(());
                }
                if self.mixer_cfg().is_some() {
                    self.touch = Some(Touch { start: (x, y), key: None, swiping: false, wake_only: false });
                    return self.mixer_down(dev, x, y);
                }
                let owners = self.owners();
                let key = self.grid.hit(x, y).map(|i| owners[i]).filter(|&i| self.page_keys().get(i).is_some_and(|k| !k.is_empty()));
                self.touch = Some(Touch { start: (x, y), key, swiping: false, wake_only: false });
                if let Some(i) = key.filter(|&i| self.widget(i).is_some()) {
                    return self.widget_down(dev, i, x, y);
                }
                // Via Wi-Fi la pressione non manda pixel: il comando non deve aspettare la rete.
                if let Some(i) = key.filter(|_| self.link == Link::Usb) {
                    self.draw_key(dev, i, true)?;
                }
            }
            Phase::Move if self.mixer_cfg().is_some() => return self.mixer_move(dev, x, y),
            Phase::Up if self.mixer_cfg().is_some() => return self.mixer_up(dev, x, y),
            Phase::Move if self.touch_widget().is_some() => return self.widget_move(dev, x, y),
            Phase::Up if self.touch_widget().is_some() => return self.widget_up(dev, x, y),
            Phase::Move => {
                let Some(t) = self.touch.as_mut() else { return Ok(()) };
                if !t.swiping && !t.wake_only && (x - t.start.0).abs() > 24 {
                    t.swiping = true;
                    if let Some(i) = t.key.filter(|_| self.link == Link::Usb) {
                        self.draw_key(dev, i, false)?;
                    }
                }
            }
            Phase::Up => {
                let Some(t) = self.touch.take() else { return Ok(()) };
                if t.wake_only {
                    return Ok(());
                }
                let (dx, dy) = (x - t.start.0, y - t.start.1);
                if t.swiping {
                    return self.swipe_page(dev, dx, dy);
                }
                if let Some(i) = t.key {
                    // Prima il comando, poi il disegno.
                    let page = self.page;
                    if self.grid.hit(x, y).map(|j| self.owners()[j]) == Some(i) {
                        self.trigger(dev, i)?;
                    }
                    if self.link == Link::Usb && self.page == page {
                        self.draw_key(dev, i, false)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn swipe_page(&mut self, dev: &mut Device, dx: i32, dy: i32) -> Result<()> {
        if dx.abs() > 60 && dx.abs() as f32 > dy.abs() as f32 * 1.5 {
            self.change_page(dev, if dx < 0 { 1 } else { -1 })?;
        }
        Ok(())
    }

    fn change_page(&mut self, dev: &mut Device, delta: isize) -> Result<()> {
        // Una scelta a mano vince sul cambio automatico.
        self.auto_from = None;
        self.set_page(dev, config::step_page(&self.cfg.pages, self.page, delta))
    }

    fn set_page(&mut self, dev: &mut Device, page: usize) -> Result<()> {
        if page != self.page {
            self.page = page;
            self.next_poll = Instant::now();
            self.mixer.next_poll = Instant::now();
            self.mixer.pressed = None;
            self.mixer.dragging = None;
            self.full_redraw(dev)?;
        }
        Ok(())
    }

    fn trigger(&mut self, dev: &mut Device, i: usize) -> Result<()> {
        let Some(key) = self.page_keys().get(i).cloned() else { return Ok(()) };
        if let Some(cmd) = &key.exec {
            spawn_shell(cmd);
            // Aggiorna presto gli stati, così i toggle si vedono subito.
            self.next_poll = Instant::now() + Duration::from_millis(300);
        }
        if !key.steps.is_empty() {
            let tx = self.tx.clone();
            macros::run(key.steps.clone(), key.step_delay, move |a| drop(tx.send(Event::Action(a))));
            self.next_poll = Instant::now() + Duration::from_millis(300);
        }
        if let Some(a) = &key.action {
            self.run_action(dev, a)?;
        }
        Ok(())
    }

    fn run_action(&mut self, dev: &mut Device, action: &str) -> Result<()> {
        match action {
            "next-page" => self.change_page(dev, 1)?,
            "prev-page" => self.change_page(dev, -1)?,
            "backlight-off" => {
                dev.backlight(false)?;
                self.backlight = false;
            }
            // Dopo questi la scheda si scollega da sola; il programma la aspetta come sempre.
            "board-restart" => dev.power(false)?,
            "board-off" => dev.power(true)?,
            "reload" => {
                self.cfg_mtime = None;
                self.scheme_mtime = None;
                self.next_file_check = Instant::now();
            }
            a => match a.strip_prefix("page:") {
                Some(name) => match self.cfg.pages.iter().position(|p| p.name == name) {
                    Some(p) => {
                        self.auto_from = None;
                        self.set_page(dev, p)?
                    }
                    None => eprintln!("{}", tr!("pagina '{}' inesistente", name)),
                },
                None => eprintln!("{}", tr!("azione sconosciuta: {}", a)),
            },
        }
        Ok(())
    }
}

/// Pagina audio.
impl App {
    fn mixer_cfg(&self) -> Option<&MixerConfig> {
        self.cfg.pages.get(self.page).filter(|p| p.kind == PageKind::Mixer).map(|p| &p.mixer)
    }

    fn mixer_layout(&self) -> Option<Layout> {
        self.mixer_cfg().map(|c| Layout::new(c, self.canvas.w, self.canvas.h, config::bar_height(&self.cfg)))
    }

    /// Disegna una parte della pagina audio sul canvas e ne restituisce il rettangolo.
    fn mixer_render(&mut self, part: Part) -> Option<Rect> {
        let l = self.mixer_layout()?;
        let m = &self.mixer;
        let v = View {
            channels: &m.channels,
            np: m.np.as_ref(),
            art: m.art.as_deref(),
            scroll: m.scroll,
            dragging: m.dragging,
            pressed: m.pressed,
        };
        let (c, f, t) = (&mut self.canvas, &self.fonts, &self.theme);
        Some(match part {
            Part::All => {
                mixer::draw_all(c, f, t, &l, &v);
                Rect { x: 0, y: 0, w: l.w, h: l.h }
            }
            Part::Card => {
                mixer::draw_card(c, f, t, &l, &v);
                l.card?
            }
            Part::Progress => {
                mixer::draw_card(c, f, t, &l, &v);
                l.progress()?
            }
            Part::Rows => {
                mixer::draw_rows(c, f, t, &l, &v);
                l.rows_area()
            }
            Part::Row(i) => {
                let slot = i.checked_sub(m.scroll).filter(|&s| s < l.visible)?;
                mixer::draw_row(c, f, t, &l, &v, slot);
                l.cell(slot, m.channels.len())
            }
        })
    }

    fn mixer_blit(&mut self, dev: &mut Device, part: Part) -> Result<()> {
        match self.mixer_render(part) {
            Some(r) => dev.blit(r.x, r.y, r.w, r.h, &self.canvas.rgb565(r.x, r.y, r.w, r.h)),
            None => Ok(()),
        }
    }

    fn spawn_mixer_poll(&self) {
        let Some(cfg) = self.poll_cfg() else { return };
        if self.mixer.busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let refresh_id = self.mixer.identity_at.is_none_or(|t| t.elapsed() > Duration::from_secs(4));
        let identity = self.mixer.identity.clone();
        let (tx, generation) = (self.tx.clone(), self.generation);
        thread::spawn(move || {
            let channels = audio::snapshot(&cfg);
            let (np, identity) = if cfg.player {
                let id = if refresh_id { media::caelestia_player() } else { identity };
                (media::query(id.as_deref()), refresh_id.then_some(id))
            } else {
                (None, None)
            };
            let _ = tx.send(Event::Mixer { generation, channels, np, identity });
        });
    }

    fn on_mixer_data(&mut self, dev: &mut Device, mut channels: Vec<Channel>, np: Option<NowPlaying>) -> Result<()> {
        if self.poll_cfg().is_none() {
            return Ok(());
        }
        let l = self.mixer_layout();
        self.mixer.held.retain(|_, (_, _, t)| t.elapsed() < HOLD);
        let dragged = self.mixer.dragging.and_then(|i| self.mixer.channels.get(i)).map(|c| c.key.clone());
        for ch in &mut channels {
            if let Some(&(v, m, _)) = self.mixer.held.get(&ch.key) {
                (ch.volume, ch.muted) = (v, m);
            }
        }
        // Se l'elenco cambia durante un trascinamento, il cursore segue il suo canale.
        self.mixer.dragging = dragged.and_then(|k| channels.iter().position(|c| c.key == k));
        if self.mixer.dragging.is_none() && self.mixer.pending.is_some() {
            self.mixer.pending = None;
        }
        if channels != self.mixer.channels {
            let old = std::mem::replace(&mut self.mixer.channels, channels);
            match l {
                Some(l) => {
                    self.mixer.scroll = self.mixer.scroll.min(l.max_scroll(self.mixer.channels.len()));
                    if !matches!(self.mixer.pressed, Some(Hit::Mute(_) | Hit::Slider(_))) {
                        self.mixer_blit(dev, Part::Rows)?;
                    }
                }
                // Nella griglia si ridisegnano solo i cursori il cui canale è cambiato.
                None => {
                    for (i, target) in self.volume_widgets() {
                        let (a, b) = (widgets::find(&old, &target).map(|j| &old[j]), self.channel_of(&target));
                        if a != b {
                            self.widget_blit(dev, i, false)?;
                        }
                    }
                }
            }
        }
        if np != self.mixer.np {
            let url = np.as_ref().map(|n| n.art_url.clone()).unwrap_or_default();
            if url != self.mixer.art_url {
                self.mixer.art_url = url.clone();
                self.mixer.art = None;
                if !url.is_empty() {
                    let tx = self.tx.clone();
                    thread::spawn(move || {
                        let px = media::load_art(&url, mixer::ART);
                        let _ = tx.send(Event::Art { url, px });
                    });
                }
            }
            // Mentre suona cambia solo la posizione: basta inviare la barra.
            let only_position = match (&self.mixer.np, &np) {
                (Some(a), Some(b)) => {
                    !a.artist.is_empty() && NowPlaying { position: b.position, ..a.clone() } == *b
                }
                _ => false,
            };
            self.mixer.np = np;
            // Via Wi-Fi l'avanzamento si aggiorna ogni 5 secondi: meno traffico per la scheda.
            if only_position && self.link == Link::Wifi && self.mixer.progress_at.is_some_and(|t| t.elapsed() < Duration::from_secs(5)) {
                return Ok(());
            }
            if only_position {
                self.mixer.progress_at = Some(Instant::now());
            }
            self.mixer_blit(dev, if only_position { Part::Progress } else { Part::Card })?;
            for i in self.player_widgets() {
                self.widget_blit(dev, i, only_position)?;
            }
        }
        Ok(())
    }

    fn set_local(&mut self, i: usize, volume: Option<f32>, muted: Option<bool>) {
        let Some(ch) = self.mixer.channels.get_mut(i) else { return };
        if let Some(v) = volume {
            ch.volume = v;
        }
        if let Some(m) = muted {
            ch.muted = m;
        }
        self.mixer.held.insert(ch.key.clone(), (ch.volume, ch.muted, Instant::now()));
    }

    fn send_volume(&mut self, i: usize) {
        self.mixer.pending = None;
        let Some(ch) = self.mixer.channels.get(i) else { return };
        audio::set_volume(&ch.target, ch.volume);
        if let Some(h) = self.mixer.held.get_mut(&ch.key) {
            h.2 = Instant::now();
        }
        self.mixer.last_set = Some(Instant::now());
    }

    fn drag_to(&mut self, dev: &mut Device, i: usize, x: i32, y: i32) -> Result<()> {
        let Some(l) = self.mixer_layout() else { return Ok(()) };
        let v = l.value_at(x, y);
        if self.mixer.channels.get(i).is_some_and(|c| c.volume != v) {
            self.set_local(i, Some(v), None);
            if self.mixer.last_set.is_none_or(|t| t.elapsed() >= SET_EVERY) {
                self.send_volume(i);
            } else {
                self.mixer.pending = Some(i);
            }
        }
        self.mixer_blit(dev, Part::Row(i))
    }

    fn mixer_down(&mut self, dev: &mut Device, x: i32, y: i32) -> Result<()> {
        let Some(l) = self.mixer_layout() else { return Ok(()) };
        match l.hit(x, y, self.mixer.scroll, self.mixer.channels.len()) {
            // In verticale si aspetta il primo movimento: su/giù è volume, di lato è scorrimento.
            Some(Hit::Slider(i)) if l.vertical => self.mixer.pressed = Some(Hit::Slider(i)),
            Some(Hit::Slider(i)) => {
                self.mixer.dragging = Some(i);
                self.drag_to(dev, i, x, y)?;
            }
            Some(h) => {
                self.mixer.pressed = Some(h);
                self.blit_hit(dev, h)?;
            }
            None => {}
        }
        Ok(())
    }

    fn blit_hit(&mut self, dev: &mut Device, h: Hit) -> Result<()> {
        match h {
            Hit::Card | Hit::Button(_) => self.mixer_blit(dev, Part::Card),
            Hit::Mute(i) | Hit::Slider(i) => self.mixer_blit(dev, Part::Row(i)),
        }
    }

    fn mixer_move(&mut self, dev: &mut Device, x: i32, y: i32) -> Result<()> {
        if let Some(i) = self.mixer.dragging {
            return self.drag_to(dev, i, x, y);
        }
        let Some(t) = self.touch.as_mut() else { return Ok(()) };
        if let Some(Hit::Slider(i)) = self.mixer.pressed {
            let (dx, dy) = ((x - t.start.0).abs(), (y - t.start.1).abs());
            if dy > 6 && dy >= dx {
                self.mixer.pressed = None;
                self.mixer.dragging = Some(i);
                return self.drag_to(dev, i, x, y);
            }
            if dx > 12 {
                self.mixer.pressed = None;
                t.swiping = true;
            }
            return Ok(());
        }
        if !t.swiping && !t.wake_only && ((x - t.start.0).abs() > 24 || (y - t.start.1).abs() > 16) {
            t.swiping = true;
            if let Some(h) = self.mixer.pressed.take() {
                self.blit_hit(dev, h)?;
            }
        }
        Ok(())
    }

    fn mixer_up(&mut self, dev: &mut Device, x: i32, y: i32) -> Result<()> {
        let Some(t) = self.touch.take() else { return Ok(()) };
        if t.wake_only {
            return Ok(());
        }
        if let Some(i) = self.mixer.dragging.take() {
            self.drag_to(dev, i, x, y)?;
            self.send_volume(i);
            return self.mixer_blit(dev, Part::Row(i));
        }
        let Some(l) = self.mixer_layout() else { return Ok(()) };
        let (dx, dy) = (x - t.start.0, y - t.start.1);
        if t.swiping {
            let horizontal = dx.abs() as f32 > dy.abs() as f32 * 1.5;
            let on_card = l.card.is_some_and(|c| t.start.1 < (c.y + c.h) as i32);
            // Con i cursori verticali lo scorrimento orizzontale sotto la scheda sposta le colonne.
            // Senza niente da scorrere, lo scorrimento orizzontale cambia pagina anche lì.
            let can_scroll = l.max_scroll(self.mixer.channels.len()) > 0;
            let scroll_d = if l.vertical && !on_card {
                (horizontal && can_scroll).then_some((dx, 63.0))
            } else {
                (!horizontal).then_some((dy, 36.0))
            };
            if horizontal && dx.abs() > 60 && scroll_d.is_none() {
                return self.change_page(dev, if dx < 0 { 1 } else { -1 });
            }
            if let Some((d, step)) = scroll_d.filter(|(d, _)| d.abs() > 20) {
                // Trascinare verso l'alto (o a sinistra) mostra i canali successivi.
                let (dy, rows) = (d, ((d.abs() as f32 / step).round() as usize).max(1));
                let max = l.max_scroll(self.mixer.channels.len());
                let s = self.mixer.scroll;
                let new = if dy < 0 { (s + rows).min(max) } else { s.saturating_sub(rows) };
                if new != s {
                    self.mixer.scroll = new;
                    self.mixer_blit(dev, Part::Rows)?;
                }
            }
            return Ok(());
        }
        let Some(h) = self.mixer.pressed.take() else { return Ok(()) };
        self.blit_hit(dev, h)?;
        if l.hit(x, y, self.mixer.scroll, self.mixer.channels.len()) != Some(h) {
            return Ok(());
        }
        let player = self.mixer.np.as_ref().map(|n| n.player.clone()).unwrap_or_default();
        match h {
            Hit::Card => spawn_shell("caelestia toggle music"),
            Hit::Button(b) => {
                let cmd = match b {
                    Button::Prev => "previous",
                    Button::PlayPause => "play-pause",
                    Button::Next => "next",
                };
                media::control(&player, cmd);
                if let (Button::PlayPause, Some(np)) = (b, self.mixer.np.as_mut()) {
                    np.playing = !np.playing;
                    self.mixer_blit(dev, Part::Card)?;
                }
                self.mixer.next_poll = Instant::now() + Duration::from_millis(300);
            }
            Hit::Mute(i) => {
                let Some(ch) = self.mixer.channels.get(i) else { return Ok(()) };
                let mute = !ch.muted;
                audio::set_mute(&ch.target, mute);
                self.set_local(i, None, Some(mute));
                self.mixer_blit(dev, Part::Row(i))?;
            }
            Hit::Slider(i) => {
                self.drag_to(dev, i, x, y)?;
                self.send_volume(i);
            }
        }
        Ok(())
    }
}

/// Parti audio nella griglia dei tasti.
impl App {
    fn owners(&self) -> Vec<usize> {
        config::owners(self.page_keys(), self.grid.cols, self.grid.rows)
    }

    /// Spazio occupato sullo schermo dal tasto `i` (più posti per le parti audio grandi).
    fn slot_area(&self, i: usize) -> Rect {
        let (w, h) = config::cells(&self.owners(), i, self.grid.cols);
        self.grid.area(i, w, h)
    }

    fn widget(&self, i: usize) -> Option<Widget> {
        self.page_keys().get(i).and_then(|k| k.widget)
    }

    /// Parte audio toccata in questo momento.
    fn touch_widget(&self) -> Option<(usize, Widget)> {
        let i = self.touch.as_ref()?.key?;
        Some((i, self.widget(i)?))
    }

    fn volume_widgets(&self) -> Vec<(usize, String)> {
        let owners = self.owners();
        self.page_keys()
            .iter()
            .enumerate()
            .filter(|&(i, k)| k.widget == Some(Widget::Volume) && owners.get(i) == Some(&i))
            .map(|(i, k)| (i, k.target.clone()))
            .collect()
    }

    /// Parti visibili di questi tipi nella pagina attuale.
    fn widgets_of(&self, kinds: &[Widget]) -> Vec<usize> {
        let owners = self.owners();
        (0..self.page_keys().len()).filter(|&i| self.widget(i).is_some_and(|w| kinds.contains(&w)) && owners.get(i) == Some(&i)).collect()
    }

    /// Rilegge i limiti di Claude se Claude Code li ha aggiornati.
    fn check_claude(&mut self) -> bool {
        let m = mtime(&claude::path());
        if m == self.claude_mtime {
            return false;
        }
        self.claude_mtime = m;
        let l = claude::load();
        let changed = l != self.claude;
        self.claude = l;
        changed
    }

    fn player_widgets(&self) -> Vec<usize> {
        let owners = self.owners();
        (0..self.page_keys().len()).filter(|&i| self.widget(i) == Some(Widget::Player) && owners.get(i) == Some(&i)).collect()
    }

    fn channel_of(&self, target: &str) -> Option<&Channel> {
        widgets::find(&self.mixer.channels, target).map(|j| &self.mixer.channels[j])
    }

    /// Cosa leggere per la pagina: quella audio, oppure i canali e il lettore delle parti nella griglia.
    fn poll_cfg(&self) -> Option<MixerConfig> {
        if let Some(c) = self.mixer_cfg() {
            return Some(c.clone());
        }
        let keys = self.page_keys();
        let owners = self.owners();
        let shown = |w: Widget| keys.iter().enumerate().any(|(i, k)| k.widget == Some(w) && owners.get(i) == Some(&i));
        let (vol, player) = (shown(Widget::Volume), shown(Widget::Player));
        let mic = keys.iter().any(|k| k.widget == Some(Widget::Volume) && k.target.trim() == "@mic");
        (vol || player).then(|| MixerConfig { player, master: true, mic, vertical: true, hide: Vec::new() })
    }

    fn render_widget(&mut self, i: usize) {
        let Some(key) = self.page_keys().get(i).cloned() else { return };
        let r = self.slot_area(i);
        let radius = self.cfg.layout.radius;
        let (c, f, t, m) = (&mut self.canvas, &self.fonts, &self.theme, &self.mixer);
        match key.widget {
            Some(Widget::Volume) => {
                let j = widgets::find(&m.channels, &key.target);
                let dragging = j.is_some() && m.dragging == j;
                widgets::draw_volume(c, f, t, r, radius, j.map(|j| &m.channels[j]), &key.target, dragging);
            }
            Some(Widget::Player) => {
                let pressed = self.touch.as_ref().filter(|t| t.key == Some(i)).and(m.pressed);
                widgets::draw_player(c, f, t, r, radius, m.np.as_ref(), m.art.as_deref(), pressed);
            }
            Some(Widget::Clock) => widgets::draw_clock(c, f, t, r, radius, &self.clock, &self.date),
            Some(Widget::ClaudeLimits) => widgets::draw_limits(c, f, t, r, radius, self.claude.as_ref(), claude::now()),
            Some(Widget::Printer) => widgets::draw_printer(c, f, t, r, radius, crate::printer::status().as_ref()),
            None => {}
        }
    }

    /// Ridisegna una parte audio; con `progress` invia solo la barra di avanzamento.
    fn widget_blit(&mut self, dev: &mut Device, i: usize, progress: bool) -> Result<()> {
        if self.mixer_cfg().is_some() {
            return Ok(());
        }
        self.render_widget(i);
        let area = self.slot_area(i);
        let r = if progress { widgets::progress(area).unwrap_or(area) } else { area };
        dev.blit(r.x, r.y, r.w, r.h, &self.canvas.rgb565(r.x, r.y, r.w, r.h))
    }

    fn widget_down(&mut self, dev: &mut Device, i: usize, x: i32, y: i32) -> Result<()> {
        self.mixer.pressed = None;
        if self.widget(i) == Some(Widget::Player) {
            self.mixer.pressed = Some(widgets::player_hit(self.slot_area(i), x, y));
            if self.link == Link::Usb {
                self.widget_blit(dev, i, false)?;
            }
        }
        // Il cursore aspetta il primo movimento: lungo il cursore è volume, di traverso è swipe.
        Ok(())
    }

    fn widget_drag(&mut self, dev: &mut Device, i: usize, x: i32, y: i32) -> Result<()> {
        let Some(target) = self.page_keys().get(i).map(|k| k.target.clone()) else { return Ok(()) };
        let Some(j) = widgets::find(&self.mixer.channels, &target) else { return Ok(()) };
        self.mixer.dragging = Some(j);
        let v = widgets::value_at(self.slot_area(i), x, y);
        if self.mixer.channels[j].volume != v {
            self.set_local(j, Some(v), None);
            if self.mixer.last_set.is_none_or(|t| t.elapsed() >= SET_EVERY) {
                self.send_volume(j);
            } else {
                self.mixer.pending = Some(j);
            }
        }
        self.widget_blit(dev, i, false)
    }

    fn widget_move(&mut self, dev: &mut Device, x: i32, y: i32) -> Result<()> {
        let Some((i, w)) = self.touch_widget() else { return Ok(()) };
        if self.mixer.dragging.is_some() {
            return self.widget_drag(dev, i, x, y);
        }
        let area = self.slot_area(i);
        let Some(t) = self.touch.as_mut() else { return Ok(()) };
        if t.swiping || t.wake_only {
            return Ok(());
        }
        let (dx, dy) = ((x - t.start.0).abs(), (y - t.start.1).abs());
        match w {
            Widget::Volume => {
                let (along, across) = if widgets::vertical(area) { (dy, dx) } else { (dx, dy) };
                if along > 6 && along >= across {
                    return self.widget_drag(dev, i, x, y);
                }
                if across > 12 {
                    t.swiping = true;
                }
            }
            Widget::Clock | Widget::ClaudeLimits | Widget::Printer => {
                if dx > 24 || dy > 16 {
                    t.swiping = true;
                }
            }
            Widget::Player => {
                if dx > 24 || dy > 16 {
                    t.swiping = true;
                    if self.mixer.pressed.take().is_some() && self.link == Link::Usb {
                        self.widget_blit(dev, i, false)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn widget_up(&mut self, dev: &mut Device, x: i32, y: i32) -> Result<()> {
        let Some((i, w)) = self.touch_widget() else { return Ok(()) };
        let Some(t) = self.touch.take() else { return Ok(()) };
        if t.wake_only {
            return Ok(());
        }
        if let Some(j) = self.mixer.dragging {
            self.widget_drag(dev, i, x, y)?;
            self.mixer.dragging = None;
            self.send_volume(j);
            return self.widget_blit(dev, i, false);
        }
        if t.swiping {
            return self.swipe_page(dev, x - t.start.0, y - t.start.1);
        }
        let area = self.slot_area(i);
        match w {
            // Un tocco secco sul cursore attiva o toglie il muto.
            Widget::Volume => {
                let target = self.page_keys()[i].target.clone();
                let Some(j) = widgets::find(&self.mixer.channels, &target) else { return Ok(()) };
                let mute = !self.mixer.channels[j].muted;
                audio::set_mute(&self.mixer.channels[j].target, mute);
                self.set_local(j, None, Some(mute));
                self.widget_blit(dev, i, false)?;
            }
            Widget::Clock | Widget::ClaudeLimits | Widget::Printer => {}
            Widget::Player => {
                let Some(h) = self.mixer.pressed.take() else { return Ok(()) };
                let player = self.mixer.np.as_ref().map(|n| n.player.clone()).unwrap_or_default();
                if widgets::player_hit(area, x, y) == h {
                    match h {
                        Hit::Button(b) => {
                            let cmd = match b {
                                Button::Prev => "previous",
                                Button::PlayPause => "play-pause",
                                Button::Next => "next",
                            };
                            media::control(&player, cmd);
                            if let (Button::PlayPause, Some(np)) = (b, self.mixer.np.as_mut()) {
                                np.playing = !np.playing;
                            }
                            self.mixer.next_poll = Instant::now() + Duration::from_millis(300);
                        }
                        _ => spawn_shell("caelestia toggle music"),
                    }
                }
                self.widget_blit(dev, i, false)?;
            }
        }
        Ok(())
    }
}

/// Batteria della scheda.
impl App {
    /// Percentuale (None se la scheda non ha batteria o non la riporta) e se è sotto carica.
    fn battery(&self) -> (Option<u8>, bool) {
        if !BATTERY {
            return (None, false);
        }
        // Sotto 2,5 V il partitore non vede una batteria: alimentazione solo da USB.
        if self.battery_mv <= 2500 {
            return (None, false);
        }
        // Con il cavo USB la scheda carica la batteria.
        let charging = self.charging || self.link == Link::Usb;
        // La curva vale a riposo: si toglie la spinta del caricatore o si rimette la caduta del carico.
        let rest = if charging { self.battery_mv.saturating_sub(CHARGE_MV) } else { self.battery_mv + LOAD_MV };
        (Some(battery_percent(rest)), charging)
    }

    fn on_battery(&mut self, dev: &mut Device, mv: u16) -> Result<()> {
        let before = self.battery();
        let prev = self.battery_mv;
        if prev > 2500 && mv > 2500 {
            if mv >= prev + PLUG_STEP_MV {
                self.charging = true;
            } else if mv + PLUG_STEP_MV <= prev {
                self.charging = false;
            }
        }
        self.battery_mv = mv;
        let now = self.battery();
        // Per gli script (es. un modulo della barra): percentuale e "charging".
        if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
            let dir = std::path::Path::new(&dir).join("cyd-deck");
            let _ = std::fs::create_dir_all(&dir);
            let text = match now {
                (Some(p), c) => format!("{p}{}\n", if c { " charging" } else { "" }),
                (None, _) => "none\n".into(),
            };
            let _ = std::fs::write(dir.join("battery"), text);
        }
        if now == before {
            return Ok(());
        }
        if self.cfg.bar.show && self.cfg.bar.battery {
            self.bar_blit(dev)?;
        }
        // Ridisegna solo i tasti che mostrano la batteria.
        let keys: Vec<usize> = self
            .page_keys()
            .iter()
            .enumerate()
            .filter(|(_, k)| [&k.icon, &k.label, &k.icon_active].into_iter().flatten().any(|t| t.contains("{battery}")))
            .map(|(i, _)| i)
            .collect();
        if self.mixer_cfg().is_none() {
            for i in keys {
                self.draw_key(dev, i, false)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery() {
        assert_eq!((battery_percent(3000), battery_percent(4200)), (0, 100));
        assert_eq!(battery_percent(3800), 52);
        assert!(battery_percent(3950) > battery_percent(3850));
        assert_eq!(expand("volume_up", Some(52), true, true), "volume_up");
        if !BATTERY {
            // Versione stabile: niente batteria, i segnaposto restano vuoti.
            assert_eq!(expand("Deck {battery}", Some(52), false, false), "Deck ");
            assert_eq!(expand("{battery}", Some(52), false, true), "");
            return;
        }
        assert_eq!(expand("{battery}", Some(52), false, false), "52%");
        assert_eq!(expand("Deck {battery}", None, false, false), "Deck --");
        assert_eq!(expand("{battery}", Some(52), false, true), "battery_4_bar");
        assert_eq!(expand("{battery}", Some(52), true, true), "battery_charging_full");
        assert_eq!(expand("volume_up", Some(52), true, true), "volume_up");
    }
}
