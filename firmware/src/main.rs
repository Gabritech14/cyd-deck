//! Firmware cyd-deck per Freenove ESP32-S3 Display 2.8" (FNK0104).
//!
//! La scheda è un "display muto": riceve dal PC rettangoli di pixel RGB565 e
//! li manda all'ILI9341, legge il touch FT6336 e restituisce gli eventi.
//! Il PC si collega via USB oppure via Wi-Fi (TCP, dopo un'autenticazione con
//! un codice condiviso). Il protocollo è descritto in host/src/device.rs.

#![no_std]
#![no_main]

use core::cell::RefCell;
use core::convert::Infallible;
use core::fmt::Write as _;
use core::sync::atomic::{AtomicI32, AtomicBool, AtomicU8, AtomicU16, AtomicU32, Ordering};

use embassy_executor::Spawner;
use embassy_futures::select::{Either, Either3, select, select3};
use embassy_net::{
    Runner, Stack, StackResources,
    tcp::TcpSocket,
    udp::{PacketMetadata, UdpSocket},
};
use embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex,
    pubsub::PubSubChannel,
    signal::Signal,
};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use embedded_graphics::{
    mono_font::{MonoTextStyle, ascii::FONT_6X13, ascii::FONT_10X20},
    pixelcolor::{Rgb565, raw::RawU16},
    prelude::*,
    text::{Alignment, Text},
};
use embedded_io_async::{Read, Write};
use esp_bootloader_esp_idf::partitions;
use esp_hal::{
    Async, Blocking,
    analog::adc::{Adc, AdcCalCurve, AdcConfig, AdcPin, Attenuation},
    clock::CpuClock,
    delay::Delay,
    gpio::{Level, Output, OutputConfig},
    i2c::master::{Config as I2cConfig, I2c},
    peripherals::{ADC1, GPIO9},
    ram,
    rng::Rng,
    spi::{
        Mode,
        master::{Config as SpiConfig, Spi},
    },
    time::Rate,
    timer::timg::TimerGroup,
    usb::usb_serial_jtag::{UsbSerialJtag, UsbSerialJtagRx, UsbSerialJtagTx},
};
use esp_radio::wifi::{
    AuthenticationMethodConfig, Config as WifiConfig, ControllerConfig, Interface, PowerSaveMode, WifiController,
    sta::StationConfig,
};
use esp_storage::FlashStorage;
use heapless::{String, Vec};
use sha2::{Digest, Sha256};
use static_cell::StaticCell;

esp_bootloader_esp_idf::esp_app_desc!();

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    esp_hal::system::software_reset()
}

const VERSION: &str = "HELLO cyd-deck 2";
const MAGIC: u8 = 0xA5;
const TOUCH_ADDR: u8 = 0x38;
/// Porta TCP del protocollo e porta UDP per farsi trovare dal PC.
const TCP_PORT: u16 = 47800;
const DISCOVERY_PORT: u16 = 47801;
/// Via USB, se un pacchetto si interrompe a metà per più di così, si torna in attesa.
/// Via TCP no: i dati non si perdono e le pause per le ritrasmissioni sono normali.
const RX_TIMEOUT: Duration = Duration::from_millis(500);
/// Il PC manda un ping ogni 10 s: senza dati per così tanto il link USB è considerato libero.
const USB_IDLE: Duration = Duration::from_secs(25);
/// Senza PC collegato lo schermo si spegne dopo questo tempo (il tocco lo riaccende).
const ALONE_BACKLIGHT: Duration = Duration::from_secs(120);
const CFG_MAGIC: &[u8; 4] = b"CYD1";

// ------------------------------------------------------------------ hardware

/// Pannello ILI9341 su SPI, senza pin di reset (si usa il reset software).
struct Display<'d> {
    spi: Spi<'d, Blocking>,
    dc: Output<'d>,
    cs: Output<'d>,
}

impl Display<'_> {
    fn cmd(&mut self, c: u8, data: &[u8]) {
        self.cs.set_low();
        self.dc.set_low();
        let _ = self.spi.write(&[c]);
        self.dc.set_high();
        if !data.is_empty() {
            let _ = self.spi.write(data);
        }
        self.cs.set_high();
    }

    fn init(&mut self, delay: &Delay) {
        self.cmd(0x01, &[]); // reset software
        delay.delay_millis(150);
        const INIT: &[(u8, &[u8])] = &[
            (0xEF, &[0x03, 0x80, 0x02]),
            (0xCF, &[0x00, 0xC1, 0x30]),
            (0xED, &[0x64, 0x03, 0x12, 0x81]),
            (0xE8, &[0x85, 0x00, 0x78]),
            (0xCB, &[0x39, 0x2C, 0x00, 0x34, 0x02]),
            (0xF7, &[0x20]),
            (0xEA, &[0x00, 0x00]),
            (0xC0, &[0x23]),       // power control 1
            (0xC1, &[0x10]),       // power control 2
            (0xC5, &[0x3E, 0x28]), // VCOM
            (0xC7, &[0x86]),
            (0x36, &[0x28]), // MADCTL: orizzontale, BGR (il PC lo reimposta)
            (0x3A, &[0x55]), // 16 bit per pixel
            (0xB1, &[0x00, 0x18]),
            (0xB6, &[0x08, 0x82, 0x27]),
            (0xF2, &[0x00]),
            (0x26, &[0x01]),
            (0xE0, &[0x0F, 0x31, 0x2B, 0x0C, 0x0E, 0x08, 0x4E, 0xF1, 0x37, 0x07, 0x10, 0x03, 0x0E, 0x09, 0x00]),
            (0xE1, &[0x00, 0x0E, 0x14, 0x03, 0x11, 0x07, 0x31, 0xC1, 0x48, 0x08, 0x0F, 0x0C, 0x31, 0x36, 0x0F]),
            (0x21, &[]), // inversione attiva, come nella config Freenove
        ];
        for (c, d) in INIT {
            self.cmd(*c, d);
        }
        self.cmd(0x11, &[]); // sleep out
        delay.delay_millis(120);
        self.cmd(0x29, &[]); // display on
    }

    /// Apre una finestra e lascia CS basso: seguono `data()` e poi `end()`.
    fn begin_window(&mut self, x: u16, y: u16, w: u16, h: u16) {
        let (x1, y1) = (x + w - 1, y + h - 1);
        self.cmd(0x2A, &[(x >> 8) as u8, x as u8, (x1 >> 8) as u8, x1 as u8]);
        self.cmd(0x2B, &[(y >> 8) as u8, y as u8, (y1 >> 8) as u8, y1 as u8]);
        self.cs.set_low();
        self.dc.set_low();
        let _ = self.spi.write(&[0x2C]);
        self.dc.set_high();
    }

    fn data(&mut self, d: &[u8]) {
        let _ = self.spi.write(d);
    }

    fn end(&mut self) {
        self.cs.set_high();
    }

    fn fill(&mut self, x: u16, y: u16, w: u16, h: u16, color: u16) {
        let mut buf = [0u8; 128];
        for px in buf.chunks_exact_mut(2) {
            px.copy_from_slice(&color.to_be_bytes());
        }
        self.begin_window(x, y, w, h);
        let mut left = w as u32 * h as u32 * 2;
        while left > 0 {
            let n = left.min(buf.len() as u32) as usize;
            self.data(&buf[..n]);
            left -= n as u32;
        }
        self.end();
    }
}

struct Hw {
    display: Display<'static>,
    backlight: Output<'static>,
}

impl Hw {
    fn set_backlight(&mut self, on: bool) {
        self.backlight.set_level(if on { Level::High } else { Level::Low });
        BACKLIGHT_ON.store(on, Ordering::Relaxed);
        BACKLIGHT.signal(on);
    }
}

const BG: u16 = 0x1881;
const TILE: u16 = 0x20E3;
const BAND_W: usize = 300;
const BAND_H: usize = 52;

/// Riquadro in RAM dove embedded-graphics scrive il testo di stato.
struct Band<'a>(&'a mut [u8]);

impl OriginDimensions for Band<'_> {
    fn size(&self) -> Size {
        Size::new(BAND_W as u32, BAND_H as u32)
    }
}

impl DrawTarget for Band<'_> {
    type Color = Rgb565;
    type Error = Infallible;

    fn draw_iter<I: IntoIterator<Item = Pixel<Rgb565>>>(&mut self, pixels: I) -> Result<(), Infallible> {
        for Pixel(p, c) in pixels {
            if p.x >= 0 && p.y >= 0 && (p.x as usize) < BAND_W && (p.y as usize) < BAND_H {
                let i = (p.y as usize * BAND_W + p.x as usize) * 2;
                self.0[i..i + 2].copy_from_slice(&RawU16::from(c).into_inner().to_be_bytes());
            }
        }
        Ok(())
    }
}

/// Schermata di attesa: griglia vuota con lo stato di rete e batteria al centro.
fn splash(hw: &mut Hw, line1: &str, line2: &str) {
    let d = &mut hw.display;
    d.fill(0, 0, 320, 240, BG);
    for i in 0..12u16 {
        d.fill(10 + (i % 4) * 77, 8 + (i / 4) * 77, 69, 69, TILE);
    }
    let mut buf = [0u8; BAND_W * BAND_H * 2];
    for px in buf.chunks_exact_mut(2) {
        px.copy_from_slice(&TILE.to_be_bytes());
    }
    let mut band = Band(&mut buf);
    let center = (BAND_W / 2) as i32;
    let fg = Rgb565::new(30, 60, 29);
    let dim = Rgb565::new(20, 42, 20);
    let _ = Text::with_alignment(line1, Point::new(center, 22), MonoTextStyle::new(&FONT_10X20, fg), Alignment::Center)
        .draw(&mut band);
    let _ = Text::with_alignment(line2, Point::new(center, 42), MonoTextStyle::new(&FONT_6X13, dim), Alignment::Center)
        .draw(&mut band);
    d.begin_window(10, 94, BAND_W as u16, BAND_H as u16);
    d.data(&buf);
    d.end();
}

/// Controller touch capacitivo FT6336 (un solo punto).
struct Touch<'d> {
    i2c: I2c<'d, Blocking>,
}

impl Touch<'_> {
    fn read(&mut self) -> Option<(u16, u16)> {
        let mut b = [0u8; 5];
        self.i2c.write_read(TOUCH_ADDR, &[0x02], &mut b).ok()?;
        let count = b[0] & 0x0F;
        if count == 0 || count > 2 {
            return None;
        }
        let x = (((b[1] & 0x0F) as u16) << 8) | b[2] as u16;
        let y = (((b[3] & 0x0F) as u16) << 8) | b[4] as u16;
        Some((x, y))
    }
}

// ------------------------------------------------------------------ stato condiviso

type Line = Vec<u8, 32>;

/// Eventi del touch, letti sia dal link USB sia da quello TCP.
static EVENTS: PubSubChannel<CriticalSectionRawMutex, Line, 16, 2, 1> = PubSubChannel::new();
/// Stato della retroilluminazione, per il risparmio energetico del Wi-Fi.
static BACKLIGHT: Signal<CriticalSectionRawMutex, bool> = Signal::new();
static BACKLIGHT_ON: AtomicBool = AtomicBool::new(true);
/// Chiede di ridisegnare la schermata di attesa.
static STATUS_DIRTY: Signal<CriticalSectionRawMutex, ()> = Signal::new();
/// 0 = Wi-Fi non configurato, 1 = connessione in corso, 2 = connesso.
static NET_STATE: AtomicU8 = AtomicU8::new(0);
static NET_IP: AtomicU32 = AtomicU32::new(0);
static BATTERY_MV: AtomicU16 = AtomicU16::new(0);
static TCP_CONNECTED: AtomicBool = AtomicBool::new(false);
/// Ultimo dato ricevuto via USB, in millisecondi dall'avvio.
static USB_SEEN_MS: AtomicU32 = AtomicU32::new(0);
/// Ultimo pacchetto ricevuto dalla rete (ricerca del PC o dati TCP), in millisecondi dall'avvio.
static NET_RX_MS: AtomicU32 = AtomicU32::new(0);
/// Potenza del segnale Wi-Fi in dBm (0 = sconosciuta).
static RSSI: AtomicI32 = AtomicI32::new(0);
/// Senza pacchetti per così tanto, a schermo acceso e senza PC, il Wi-Fi si ricollega:
/// il PC cerca la scheda ogni pochi secondi, quindi il silenzio vuol dire che non riceve più.
const NET_DEAF: Duration = Duration::from_secs(45);

fn now_ms() -> u32 {
    (Instant::now().as_millis() as u32).max(1)
}

fn net_seen() {
    NET_RX_MS.store(now_ms(), Ordering::Relaxed);
}

fn usb_active() -> bool {
    let seen = USB_SEEN_MS.load(Ordering::Relaxed);
    seen != 0 && (Instant::now().as_millis() as u32).wrapping_sub(seen) < USB_IDLE.as_millis() as u32
}

fn host_active() -> bool {
    TCP_CONNECTED.load(Ordering::Relaxed) || usb_active()
}

/// Rete salvata in flash, impostata dal PC via USB.
struct WifiCfg {
    ssid: String<32>,
    password: String<64>,
    token: String<64>,
    /// Impronta della configurazione: il PC la confronta con la sua per sapere se va reinviata.
    id: String<16>,
}

fn parse_cfg(payload: &[u8]) -> Option<WifiCfg> {
    let mut it = payload.split(|&b| b == 0);
    let mut field = || core::str::from_utf8(it.next()?).ok();
    let (ssid, password, token, id) = (field()?, field()?, field()?, field()?);
    if ssid.is_empty() || token.is_empty() {
        return None;
    }
    Some(WifiCfg {
        ssid: ssid.try_into().ok()?,
        password: password.try_into().ok()?,
        token: token.try_into().ok()?,
        id: id.try_into().ok()?,
    })
}

/// La configurazione sta all'inizio della partizione NVS (che questo firmware non usa altrimenti).
fn with_nvs<R>(flash: &mut FlashStorage<'static>, f: impl FnOnce(&mut partitions::FlashRegion<'_, 'static>) -> R) -> Option<R> {
    let mut pt_mem = [0u8; partitions::PARTITION_TABLE_MAX_LEN];
    let pt = partitions::read_partition_table(flash, &mut pt_mem).ok()?;
    let nvs = pt.find_partition(partitions::PartitionType::Data(partitions::DataPartitionSubType::Nvs)).ok()??;
    let mut region = nvs.as_flash_region(flash);
    Some(f(&mut region))
}

fn load_cfg(flash: &mut FlashStorage<'static>) -> Option<WifiCfg> {
    let mut buf = [0u8; 6 + 256];
    with_nvs(flash, |r| r.read(0, &mut buf).ok())??;
    if &buf[..4] != CFG_MAGIC {
        return None;
    }
    let len = u16::from_le_bytes([buf[4], buf[5]]) as usize;
    parse_cfg(buf.get(6..6 + len)?)
}

/// Salva (o, con payload vuoto, cancella) la configurazione Wi-Fi.
fn save_cfg(flash: &mut FlashStorage<'static>, payload: &[u8]) -> bool {
    let mut buf = [0xFFu8; 6 + 256];
    if !payload.is_empty() {
        buf[..4].copy_from_slice(CFG_MAGIC);
        buf[4..6].copy_from_slice(&(payload.len() as u16).to_le_bytes());
        buf[6..6 + payload.len()].copy_from_slice(payload);
    }
    with_nvs(flash, |r| r.write(0, &buf).is_ok()).unwrap_or(false)
}

// ------------------------------------------------------------------ protocollo

enum State {
    Idle,
    Command,
    Args { cmd: u8, buf: [u8; 8], have: usize, need: usize },
    Pixels { left: u32 },
    Blob { buf: Vec<u8, 256>, left: usize },
}

fn arg_len(cmd: u8) -> Option<usize> {
    match cmd {
        0x01 => Some(8),               // blit: x y w h
        0x02 | 0x04 | 0x05 => Some(1), // retroilluminazione, MADCTL, inversione
        0x03 | 0x07 => Some(0),        // ping, stato della rete
        0x06 => Some(2),               // configurazione Wi-Fi: lunghezza, poi i dati
        _ => None,
    }
}

/// Risposte da mandare al PC dopo un blocco di dati.
#[derive(Default)]
struct Replies {
    hello: bool,
    net: bool,
    wifi: Option<Vec<u8, 256>>,
}

struct Parser {
    state: State,
    last: Instant,
    timeout: Option<Duration>,
}

impl Parser {
    fn new(timeout: Option<Duration>) -> Self {
        Self { state: State::Idle, last: Instant::now(), timeout }
    }

    fn feed(&mut self, hw: &RefCell<Hw>, mut data: &[u8]) -> Replies {
        let mut out = Replies::default();
        let mut hw = hw.borrow_mut();
        if !matches!(self.state, State::Idle) && self.timeout.is_some_and(|t| self.last.elapsed() > t) {
            if matches!(self.state, State::Pixels { .. }) {
                hw.display.end();
            }
            self.state = State::Idle;
        }
        self.last = Instant::now();
        while !data.is_empty() {
            let state = core::mem::replace(&mut self.state, State::Idle);
            self.state = match state {
                State::Idle => {
                    let b = data[0];
                    data = &data[1..];
                    if b == MAGIC { State::Command } else { State::Idle }
                }
                State::Command => {
                    let cmd = data[0];
                    data = &data[1..];
                    match arg_len(cmd) {
                        Some(0) => {
                            match cmd {
                                0x03 => out.hello = true,
                                _ => out.net = true,
                            }
                            State::Idle
                        }
                        Some(need) => State::Args { cmd, buf: [0; 8], have: 0, need },
                        None => State::Idle,
                    }
                }
                State::Args { cmd, mut buf, mut have, need } => {
                    let take = (need - have).min(data.len());
                    buf[have..have + take].copy_from_slice(&data[..take]);
                    have += take;
                    data = &data[take..];
                    if have < need {
                        State::Args { cmd, buf, have, need }
                    } else {
                        match cmd {
                            0x01 => {
                                let v = |i: usize| u16::from_le_bytes([buf[i], buf[i + 1]]);
                                let (x, y, w, h) = (v(0), v(2), v(4), v(6));
                                if w == 0 || h == 0 {
                                    State::Idle
                                } else {
                                    hw.display.begin_window(x, y, w, h);
                                    State::Pixels { left: w as u32 * h as u32 * 2 }
                                }
                            }
                            0x02 => {
                                hw.set_backlight(buf[0] != 0);
                                State::Idle
                            }
                            0x04 => {
                                hw.display.cmd(0x36, &buf[..1]);
                                State::Idle
                            }
                            0x05 => {
                                hw.display.cmd(if buf[0] != 0 { 0x21 } else { 0x20 }, &[]);
                                State::Idle
                            }
                            0x06 => {
                                let len = u16::from_le_bytes([buf[0], buf[1]]) as usize;
                                if len == 0 {
                                    out.wifi = Some(Vec::new());
                                    State::Idle
                                } else if len <= 256 {
                                    State::Blob { buf: Vec::new(), left: len }
                                } else {
                                    State::Idle
                                }
                            }
                            _ => State::Idle,
                        }
                    }
                }
                State::Pixels { left } => {
                    let take = (left as usize).min(data.len());
                    hw.display.data(&data[..take]);
                    data = &data[take..];
                    let left = left - take as u32;
                    if left == 0 {
                        hw.display.end();
                        State::Idle
                    } else {
                        State::Pixels { left }
                    }
                }
                State::Blob { mut buf, left } => {
                    let take = left.min(data.len());
                    let _ = buf.extend_from_slice(&data[..take]);
                    data = &data[take..];
                    if left == take {
                        out.wifi = Some(buf);
                        State::Idle
                    } else {
                        State::Blob { buf, left: left - take }
                    }
                }
            };
        }
        out
    }
}

fn hello_lines() -> String<64> {
    let mut s = String::new();
    let _ = write!(s, "{VERSION}\nB {}\n", BATTERY_MV.load(Ordering::Relaxed));
    s
}

fn net_line(cfg: Option<&WifiCfg>) -> String<64> {
    let mut s = String::new();
    let state = ["off", "connecting", "up"][NET_STATE.load(Ordering::Relaxed).min(2) as usize];
    let ip = NET_IP.load(Ordering::Relaxed).to_be_bytes();
    let id = cfg.map_or("-", |c| c.id.as_str());
    let _ = write!(s, "NET {state} {}.{}.{}.{} {id}\n", ip[0], ip[1], ip[2], ip[3]);
    s
}

fn touch_line(kind: u8, x: u16, y: u16) -> Line {
    let mut s: String<32> = String::new();
    let _ = write!(s, "{} {x} {y}\n", kind as char);
    Vec::from_slice(s.as_bytes()).unwrap_or_default()
}

// ------------------------------------------------------------------ task

/// Invia senza bloccare: se il PC non legge, i dati vengono scartati.
fn usb_send(tx: &mut UsbSerialJtagTx<'static, Async>, line: &[u8]) {
    for &b in line {
        if tx.write_byte_nb(b).is_err() {
            break;
        }
    }
    let _ = tx.flush_tx_nb();
}

#[embassy_executor::task]
async fn usb_task(
    mut rx: UsbSerialJtagRx<'static, Async>,
    mut tx: UsbSerialJtagTx<'static, Async>,
    hw: &'static RefCell<Hw>,
    flash: &'static RefCell<FlashStorage<'static>>,
    cfg: Option<&'static WifiCfg>,
) {
    let mut parser = Parser::new(Some(RX_TIMEOUT));
    let Ok(mut sub) = EVENTS.subscriber() else { return };
    let mut buf = [0u8; 512];
    loop {
        match select(rx.read(&mut buf), sub.next_message_pure()).await {
            Either::First(Ok(n)) if n > 0 => {
                USB_SEEN_MS.store((Instant::now().as_millis() as u32).max(1), Ordering::Relaxed);
                let r = parser.feed(hw, &buf[..n]);
                if r.hello {
                    usb_send(&mut tx, hello_lines().as_bytes());
                }
                if r.net {
                    usb_send(&mut tx, net_line(cfg).as_bytes());
                }
                if let Some(payload) = r.wifi {
                    let ok = save_cfg(&mut flash.borrow_mut(), &payload);
                    usb_send(&mut tx, if ok { b"OK wifi\n" } else { b"ERR wifi\n" });
                    if ok {
                        // Riparte con la nuova rete.
                        Timer::after_millis(300).await;
                        esp_hal::system::software_reset();
                    }
                }
            }
            Either::First(_) => {}
            // Gli eventi vanno sull'USB solo se lì c'è un PC: altrimenti resterebbero
            // nel buffer e arriverebbero, vecchi, al prossimo collegamento.
            Either::Second(line) if usb_active() => usb_send(&mut tx, &line),
            Either::Second(_) => {}
        }
    }
}

#[embassy_executor::task]
async fn touch_task(mut touch: Touch<'static>, hw: &'static RefCell<Hw>) {
    let Ok(publisher) = EVENTS.publisher() else { return };
    let mut pressed: Option<(u16, u16)> = None;
    loop {
        Timer::after_millis(10).await;
        let now = touch.read();
        // Senza PC il primo tocco riaccende soltanto lo schermo.
        if now.is_some() && pressed.is_none() && !BACKLIGHT_ON.load(Ordering::Relaxed) && !host_active() {
            hw.borrow_mut().set_backlight(true);
            STATUS_DIRTY.signal(());
        }
        let line = match (now, pressed) {
            (Some((x, y)), None) => Some(touch_line(b'D', x, y)),
            (Some((x, y)), Some((px, py))) if x.abs_diff(px) >= 3 || y.abs_diff(py) >= 3 => Some(touch_line(b'M', x, y)),
            (Some(_), Some(p)) => {
                // Movimento troppo piccolo: si tiene la posizione precedente.
                pressed = Some(p);
                continue;
            }
            (None, Some((px, py))) => Some(touch_line(b'U', px, py)),
            (None, None) => None,
        };
        pressed = now;
        if let Some(l) = line {
            publisher.publish_immediate(l);
        }
    }
}

/// Batteria su GPIO9, dietro un partitore 200K/200K (la tensione letta è la metà).
#[embassy_executor::task]
async fn battery_task(
    mut adc: Adc<'static, ADC1<'static>, Blocking>,
    mut pin: AdcPin<GPIO9<'static>, ADC1<'static>, AdcCalCurve<ADC1<'static>>>,
) {
    loop {
        let mut sum = 0u32;
        for _ in 0..16 {
            sum += adc.read_blocking(&mut pin) as u32;
        }
        let mv = (sum / 16 * 2) as u16;
        let old = BATTERY_MV.swap(mv, Ordering::Relaxed);
        if old.abs_diff(mv) > 30 {
            STATUS_DIRTY.signal(());
        }
        Timer::after_secs(5).await;
    }
}

/// Mostra lo stato quando nessun PC è collegato e spegne lo schermo se resta da solo.
#[embassy_executor::task]
async fn status_task(hw: &'static RefCell<Hw>, cfg: Option<&'static WifiCfg>) {
    let mut alone_since = Some(Instant::now());
    let mut shown: Option<(String<48>, String<48>)> = None;
    loop {
        let _ = with_timeout(Duration::from_secs(2), STATUS_DIRTY.wait()).await;
        if host_active() {
            alone_since = None;
            shown = None;
            continue;
        }
        let since = *alone_since.get_or_insert_with(Instant::now);
        if BACKLIGHT_ON.load(Ordering::Relaxed) && since.elapsed() > ALONE_BACKLIGHT {
            hw.borrow_mut().set_backlight(false);
        }
        if !BACKLIGHT_ON.load(Ordering::Relaxed) {
            // Al risveglio si riparte a contare.
            alone_since = Some(Instant::now());
            continue;
        }
        let mut l1: String<48> = String::new();
        let mut l2: String<48> = String::new();
        let ip = NET_IP.load(Ordering::Relaxed).to_be_bytes();
        match (cfg, NET_STATE.load(Ordering::Relaxed)) {
            (None, _) => {
                let _ = write!(l1, "In attesa del PC");
                let _ = write!(l2, "Collegala via USB");
            }
            (Some(c), 2) => {
                let _ = write!(l1, "{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);
                let _ = write!(l2, "Wi-Fi {} - in attesa del PC", c.ssid);
            }
            (Some(c), _) => {
                let _ = write!(l1, "Connessione Wi-Fi...");
                let _ = write!(l2, "{}", c.ssid);
            }
        }
        let mv = BATTERY_MV.load(Ordering::Relaxed);
        // Con la sola alimentazione USB il partitore legge poco o niente.
        if mv > 2500 {
            let _ = write!(l2, "  |  {}.{:02} V", mv / 1000, mv % 1000 / 10);
        }
        let rssi = RSSI.load(Ordering::Relaxed);
        if rssi != 0 && NET_STATE.load(Ordering::Relaxed) == 2 {
            let _ = write!(l1, "  {rssi} dBm");
        }
        if shown.as_ref() != Some(&(l1.clone(), l2.clone())) {
            splash(&mut hw.borrow_mut(), &l1, &l2);
            shown = Some((l1, l2));
        }
    }
}

#[embassy_executor::task]
async fn wifi_task(mut ctl: WifiController<'static>) {
    loop {
        NET_STATE.store(1, Ordering::Relaxed);
        STATUS_DIRTY.signal(());
        if ctl.connect_async().await.is_ok() {
            // Niente risparmio energetico: in risparmio la scheda perdeva pacchetti e restava sorda.
            let _ = ctl.set_power_saving(PowerSaveMode::None);
            net_seen();
            loop {
                match select3(ctl.wait_for_disconnect_async(), BACKLIGHT.wait(), Timer::after_secs(5)).await {
                    Either3::First(_) => break,
                    Either3::Second(_) => net_seen(),
                    Either3::Third(_) => {
                        RSSI.store(ctl.rssi().unwrap_or(0), Ordering::Relaxed);
                        STATUS_DIRTY.signal(());
                        let quiet = now_ms().wrapping_sub(NET_RX_MS.load(Ordering::Relaxed));
                        let deaf = quiet > NET_DEAF.as_millis() as u32;
                        if deaf && !TCP_CONNECTED.load(Ordering::Relaxed) && BACKLIGHT_ON.load(Ordering::Relaxed) {
                            let _ = with_timeout(Duration::from_secs(5), ctl.disconnect_async()).await;
                            break;
                        }
                    }
                }
            }
        }
        NET_STATE.store(1, Ordering::Relaxed);
        NET_IP.store(0, Ordering::Relaxed);
        STATUS_DIRTY.signal(());
        Timer::after_secs(3).await;
    }
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, Interface>) {
    runner.run().await
}

fn hex_digest(parts: &[&[u8]]) -> String<64> {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    let mut s = String::new();
    for b in h.finalize() {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn nonce() -> String<16> {
    let rng = Rng::new();
    let mut s = String::new();
    let _ = write!(s, "{:08x}{:08x}", rng.random(), rng.random());
    s
}

async fn read_line(sock: &mut TcpSocket<'_>) -> Option<Vec<u8, 200>> {
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    loop {
        match sock.read(&mut b).await {
            Ok(1) if b[0] == b'\n' => return Some(line),
            Ok(1) => line.push(b[0]).ok()?,
            _ => return None,
        }
    }
}

/// Autenticazione reciproca: entrambi dimostrano di conoscere il codice senza inviarlo.
async fn authenticate(sock: &mut TcpSocket<'_>, cfg: &WifiCfg) -> bool {
    let nb = nonce();
    let mut hello: String<64> = String::new();
    let _ = write!(hello, "CYD 2 {nb} {}\n", cfg.id);
    if sock.write_all(hello.as_bytes()).await.is_err() {
        return false;
    }
    let Ok(Some(line)) = with_timeout(Duration::from_secs(3), read_line(sock)).await else { return false };
    let Ok(line) = core::str::from_utf8(&line) else { return false };
    let mut it = line.split(' ');
    let (Some("AUTH"), Some(proof), Some(nh)) = (it.next(), it.next(), it.next()) else { return false };
    let token = cfg.token.as_bytes();
    let expected = hex_digest(&[token, b":", nb.as_bytes(), b":host"]);
    // Confronto a tempo costante.
    if proof.len() != expected.len() || proof.bytes().zip(expected.bytes()).fold(0, |a, (x, y)| a | (x ^ y)) != 0 {
        return false;
    }
    let mut ok: String<80> = String::new();
    let _ = write!(ok, "OK {}\n", hex_digest(&[token, b":", nh.as_bytes(), b":board"]));
    sock.write_all(ok.as_bytes()).await.is_ok()
}

#[embassy_executor::task]
async fn tcp_task(stack: Stack<'static>, hw: &'static RefCell<Hw>, cfg: &'static WifiCfg) {
    // Un buffer grande permette al PC di mandare più dati senza attendere le conferme.
    static RX: StaticCell<[u8; 32768]> = StaticCell::new();
    static TX: StaticCell<[u8; 1024]> = StaticCell::new();
    let (rx, tx) = (RX.init([0; 32768]), TX.init([0; 1024]));
    let Ok(mut sub) = EVENTS.subscriber() else { return };
    let mut buf = [0u8; 4096];
    loop {
        stack.wait_config_up().await;
        if let Some(c) = stack.config_v4() {
            NET_IP.store(u32::from_be_bytes(c.address.address().octets()), Ordering::Relaxed);
            NET_STATE.store(2, Ordering::Relaxed);
            STATUS_DIRTY.signal(());
        }
        let mut sock = TcpSocket::new(stack, rx, tx);
        // I keep-alive tengono viva la connessione inattiva e chiudono quelle di un PC sparito.
        sock.set_keep_alive(Some(Duration::from_secs(5)));
        sock.set_timeout(Some(Duration::from_secs(30)));
        sock.set_nagle_enabled(false);
        if sock.accept(TCP_PORT).await.is_err() {
            continue;
        }
        if authenticate(&mut sock, cfg).await {
            TCP_CONNECTED.store(true, Ordering::Relaxed);
            // Scarta i tocchi arrivati prima del collegamento.
            while sub.try_next_message_pure().is_some() {}
            let mut parser = Parser::new(None);
            loop {
                match select(sock.read(&mut buf), sub.next_message_pure()).await {
                    Either::First(Ok(n)) if n > 0 => {
                        net_seen();
                        let r = parser.feed(hw, &buf[..n]);
                        let mut fail = false;
                        if r.hello {
                            fail |= sock.write_all(hello_lines().as_bytes()).await.is_err();
                        }
                        if r.net {
                            fail |= sock.write_all(net_line(Some(cfg)).as_bytes()).await.is_err();
                        }
                        if fail {
                            break;
                        }
                    }
                    Either::First(_) => break,
                    Either::Second(line) => {
                        if sock.write_all(&line).await.is_err() {
                            break;
                        }
                    }
                }
            }
            TCP_CONNECTED.store(false, Ordering::Relaxed);
            STATUS_DIRTY.signal(());
        }
        sock.abort();
        let _ = sock.flush().await;
    }
}

/// Risponde a "CYD?" in broadcast UDP: così il PC trova la scheda senza conoscerne l'IP.
#[embassy_executor::task]
async fn discovery_task(stack: Stack<'static>, cfg: &'static WifiCfg) {
    let mut rx_meta = [PacketMetadata::EMPTY; 4];
    let mut tx_meta = [PacketMetadata::EMPTY; 4];
    let mut rx_buf = [0u8; 256];
    let mut tx_buf = [0u8; 256];
    let mut sock = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    if sock.bind(DISCOVERY_PORT).is_err() {
        return;
    }
    let mut buf = [0u8; 64];
    let mut reply: String<48> = String::new();
    let _ = write!(reply, "CYD {} {TCP_PORT}\n", cfg.id);
    loop {
        if let Ok((n, meta)) = sock.recv_from(&mut buf).await {
            net_seen();
            if buf[..n].starts_with(b"CYD?") {
                let _ = sock.send_to(reply.as_bytes(), meta.endpoint).await;
            }
        }
    }
}

// ------------------------------------------------------------------ avvio

#[esp_hal::main]
async fn main(spawner: Spawner) -> ! {
    let p = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 64 * 1024);
    // I buffer di ricezione del Wi-Fi stanno nello heap.
    esp_alloc::heap_allocator!(size: 112 * 1024);
    let timg0 = TimerGroup::new(p.TIMG0);
    esp_rtos::start(timg0.timer0, p.FROM_CPU_INTR0);
    let delay = Delay::new();

    let backlight = Output::new(p.GPIO45, Level::Low, OutputConfig::default());
    let spi = Spi::new(p.SPI2, SpiConfig::default().with_frequency(Rate::from_mhz(40)).with_mode(Mode::_0))
        .unwrap()
        .with_sck(p.GPIO12)
        .with_mosi(p.GPIO11)
        .with_miso(p.GPIO13);
    let mut display = Display {
        spi,
        dc: Output::new(p.GPIO46, Level::High, OutputConfig::default()),
        cs: Output::new(p.GPIO10, Level::High, OutputConfig::default()),
    };
    display.init(&delay);
    static HW: StaticCell<RefCell<Hw>> = StaticCell::new();
    let hw = &*HW.init(RefCell::new(Hw { display, backlight }));
    hw.borrow_mut().set_backlight(true);

    let mut touch_rst = Output::new(p.GPIO18, Level::Low, OutputConfig::default());
    delay.delay_millis(10);
    touch_rst.set_high();
    delay.delay_millis(300);
    let i2c = I2c::new(p.I2C0, I2cConfig::default().with_frequency(Rate::from_khz(400)))
        .unwrap()
        .with_sda(p.GPIO16)
        .with_scl(p.GPIO15);
    // Il pin di reset deve restare alto per sempre.
    core::mem::forget(touch_rst);

    static FLASH: StaticCell<RefCell<FlashStorage<'static>>> = StaticCell::new();
    let flash = &*FLASH.init(RefCell::new(FlashStorage::new(p.FLASH)));
    static CFG: StaticCell<Option<WifiCfg>> = StaticCell::new();
    let cfg: Option<&'static WifiCfg> = CFG.init(load_cfg(&mut flash.borrow_mut())).as_ref();

    let mut adc_cfg = AdcConfig::new();
    let bat_pin = adc_cfg.enable_pin_with_cal::<_, AdcCalCurve<ADC1>>(p.GPIO9, Attenuation::_11dB);
    let adc = Adc::new(p.ADC1, adc_cfg);

    let (usb_rx, usb_tx) = UsbSerialJtag::new(p.USB_DEVICE).into_async().split();
    spawner.spawn(usb_task(usb_rx, usb_tx, hw, flash, cfg).unwrap());
    spawner.spawn(touch_task(Touch { i2c }, hw).unwrap());
    spawner.spawn(battery_task(adc, bat_pin).unwrap());
    spawner.spawn(status_task(hw, cfg).unwrap());
    STATUS_DIRTY.signal(());

    if let Some(cfg) = cfg {
        let auth = if cfg.password.is_empty() {
            AuthenticationMethodConfig::Open
        } else {
            match cfg.password.as_str().try_into() {
                Ok(pw) => AuthenticationMethodConfig::WpaWpa2Personal(pw),
                Err(_) => AuthenticationMethodConfig::Open,
            }
        };
        let station = StationConfig::default().with_authentication(auth);
        let station = match cfg.ssid.as_str().try_into() {
            Ok(ssid) => station.with_ssid(ssid),
            Err(_) => station,
        };
        // Code più lunghe delle predefinite (5 pacchetti): mentre la scheda scrive sul display
        // i pacchetti in arrivo aspettano invece di essere scartati, evitando le ritrasmissioni.
        let radio = ControllerConfig::default()
            .with_rx_queue_size(24)
            .with_static_rx_buf_num(16)
            .with_dynamic_rx_buf_num(48)
            .with_rx_ba_win(12)
            .with_initial_config(WifiConfig::Station(station));
        match WifiController::new(p.WIFI, radio) {
            Ok(ctl) => {
                let rng = Rng::new();
                let seed = (rng.random() as u64) << 32 | rng.random() as u64;
                static RESOURCES: StaticCell<StackResources<4>> = StaticCell::new();
                let (stack, runner) = embassy_net::new(
                    Interface::station(),
                    embassy_net::Config::dhcpv4(Default::default()),
                    RESOURCES.init(StackResources::new()),
                    seed,
                );
                spawner.spawn(wifi_task(ctl).unwrap());
                spawner.spawn(net_task(runner).unwrap());
                spawner.spawn(tcp_task(stack, hw, cfg).unwrap());
                spawner.spawn(discovery_task(stack, cfg).unwrap());
            }
            Err(_) => NET_STATE.store(0, Ordering::Relaxed),
        }
    }

    loop {
        Timer::after_secs(3600).await;
    }
}
