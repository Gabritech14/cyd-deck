//! Editor grafico stile Stream Deck: modifica la config e il demone la ricarica da solo.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Result, anyhow};
use eframe::egui::{
    self, Align, Align2, Color32, ColorImage, CornerRadius, FontData, FontFamily, FontId, Id, LayerId, Layout, Order,
    Rect, RichText, Sense, Stroke, StrokeKind, TextEdit, TextureHandle, TextureOptions, load::SizedTexture,
    pos2, vec2,
};

use eframe::egui::containers::menu::{MenuButton, MenuConfig};
use eframe::egui::PopupCloseBehavior;

use crate::app::{self, Grid};
use crate::audio::{self, Channel, Target};
use crate::bar;
use crate::config::{self, CloseScope, Config, Key, MixerConfig, Page, PageKind, Step, Widget};
use crate::device;
use crate::ha;
use crate::icons;
use crate::macros;
use crate::media::{self, NowPlaying};
use crate::mixer;
use crate::presets::{self, DesktopApp, Plugin, Preset};
use crate::printer;
use crate::render::{self, Canvas, Fonts, KeyVisual, Rgb, Style};
use crate::secrets;
use crate::theme::{self, Theme};
use crate::widgets;

const AUTOSAVE_DELAY: Duration = Duration::from_millis(600);
const ICON_RESULTS: usize = 400;

pub fn run(cfg_path: &Path) -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("cyd-deck")
            .with_app_id("cyd-deck")
            .with_inner_size([1180.0, 720.0])
            .with_min_inner_size([900.0, 560.0]),
        ..Default::default()
    };
    let path = cfg_path.to_path_buf();
    eframe::run_native(
        "cyd-deck",
        options,
        Box::new(move |cc| Ok(Box::new(Editor::new(cc, path).map_err(|e| e.into_boxed_dyn_error())?))),
    )
    .map_err(|e| anyhow!("{e}"))
}

fn c32(c: Rgb) -> Color32 {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    Color32::from_rgb(q(c[0]), q(c[1]), q(c[2]))
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// Campo di testo legato a un Option<String>: vuoto = None.
fn opt_edit(ui: &mut egui::Ui, value: &mut Option<String>, hint: &str, code: bool, rows: usize) -> egui::Response {
    let mut s = value.clone().unwrap_or_default();
    let mut te = if rows > 1 { TextEdit::multiline(&mut s).desired_rows(rows) } else { TextEdit::singleline(&mut s) };
    te = te.hint_text(hint.to_string()).desired_width(f32::INFINITY);
    if code {
        te = te.code_editor();
    }
    let r = ui.add(te);
    if r.changed() {
        *value = (!s.is_empty()).then_some(s);
    }
    r
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Keys,
    Store,
    Settings,
}

#[derive(Clone, Copy, PartialEq)]
enum IconTarget {
    Normal,
    Active,
}

#[derive(Clone, Copy, PartialEq)]
enum ActionKind {
    None,
    Command,
    Macro,
    Page,
    NextPage,
    PrevPage,
    BacklightOff,
    Reload,
}

impl ActionKind {
    const ALL: [ActionKind; 8] = [
        ActionKind::None,
        ActionKind::Command,
        ActionKind::Macro,
        ActionKind::Page,
        ActionKind::NextPage,
        ActionKind::PrevPage,
        ActionKind::BacklightOff,
        ActionKind::Reload,
    ];

    fn of(k: &Key) -> Self {
        if !k.steps.is_empty() {
            return Self::Macro;
        }
        match k.action.as_deref() {
            Some("next-page") => Self::NextPage,
            Some("prev-page") => Self::PrevPage,
            Some("backlight-off") => Self::BacklightOff,
            Some("reload") => Self::Reload,
            Some(a) if a.starts_with("page:") => Self::Page,
            _ if k.exec.is_some() => Self::Command,
            _ => Self::None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::None => tr!("Nessuna"),
            Self::Command => tr!("Esegui comando"),
            Self::Macro => tr!("Macro (più azioni in fila)"),
            Self::Page => tr!("Vai a una pagina"),
            Self::NextPage => tr!("Pagina successiva"),
            Self::PrevPage => tr!("Pagina precedente"),
            Self::BacklightOff => tr!("Spegni lo schermo"),
            Self::Reload => tr!("Ricarica config"),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum StepKind {
    CloseWindows,
    Exec,
    Workspace,
    Delay,
    Action,
}

impl StepKind {
    const ALL: [StepKind; 5] = [StepKind::CloseWindows, StepKind::Exec, StepKind::Workspace, StepKind::Delay, StepKind::Action];

    fn of(s: &Step) -> Self {
        match s {
            Step::Exec { .. } => Self::Exec,
            Step::Workspace { .. } => Self::Workspace,
            Step::Delay { .. } => Self::Delay,
            Step::Action { .. } => Self::Action,
            Step::CloseWindows { .. } => Self::CloseWindows,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Exec => tr!("Apri app / comando"),
            Self::Workspace => tr!("Vai al workspace"),
            Self::Delay => tr!("Attendi"),
            Self::Action => tr!("Azione del deck"),
            Self::CloseWindows => tr!("Chiudi finestre"),
        }
    }

    fn new_step(self) -> Step {
        match self {
            Self::Exec => Step::Exec { exec: String::new(), workspace: None, background: false, wait: false },
            Self::Workspace => Step::Workspace { workspace: "1".into() },
            Self::Delay => Step::Delay { ms: 500 },
            Self::Action => Step::Action { action: "next-page".into() },
            Self::CloseWindows => Step::CloseWindows { scope: CloseScope::Current, workspace: None, special: false },
        }
    }
}

struct Editor {
    cfg_path: PathBuf,
    cfg: Config,
    saved: Config,
    disk_mtime: Option<SystemTime>,
    scheme_path: PathBuf,
    scheme_mtime: Option<SystemTime>,
    theme: Theme,
    fonts: Fonts,
    icon_names: Vec<String>,
    apps: Vec<DesktopApp>,
    /// Funzioni dello store (preset e plugin) ed errori dei file plugin.
    catalog: Vec<Plugin>,
    catalog_errors: Vec<String>,
    store_search: String,
    /// Categoria scelta nello store (None = tutte).
    store_cat: Option<String>,
    /// Pagina su cui lo store aggiunge i tasti.
    store_page: usize,
    /// Token di Home Assistant mentre lo si scrive: va nel portachiavi e poi si svuota.
    ha_token_input: String,
    /// Se nel portachiavi c'è un token (None = non ancora controllato).
    ha_token_saved: Option<bool>,
    ha: Arc<Mutex<HaState>>,
    keyring: Arc<Mutex<KeyringState>>,
    /// Codice LAN della stampante mentre lo si scrive (poi va nel portachiavi).
    bambu_code_input: String,
    bambu_code_saved: Option<bool>,
    /// Prova di collegamento alla stampante: (in corso, esito).
    printer_check: Arc<Mutex<(bool, Option<Result<String, String>>)>>,

    /// Passo della guida rapida aperta (None = chiusa).
    intro: Option<usize>,
    /// Scelta delle pagine nella guida: 0 = tieni, 1 = esempio, 2 = vuota.
    intro_pages: usize,
    /// Esito dell'ultimo controllo nella guida: (porta USB della scheda, programma in background attivo).
    intro_check: Option<(Option<String>, bool)>,
    tab: Tab,
    page: usize,
    selected: Option<usize>,
    drag_from: Option<usize>,
    clipboard: Option<Key>,
    picker: Option<IconTarget>,
    icon_search: String,
    app_search: String,
    /// App da aggiungere all'apertura automatica della pagina.
    auto_new: String,
    clock: String,
    /// Posizione dei tasti nell'ultimo frame (serve ai test).
    slot_rects: Vec<Rect>,
    test_result: Arc<Mutex<Option<String>>>,

    autosave: bool,
    last_edit: Option<Instant>,
    next_file_check: Instant,
    status: Option<(String, bool, Instant)>,

    textures: HashMap<String, TextureHandle>,
    used: HashSet<String>,

    live: Arc<Mutex<MixerLive>>,
    live_busy: Arc<AtomicBool>,
    live_next: Instant,
}

/// Stato del portachiavi dedicato; le finestre di sblocco girano in background.
#[derive(Default)]
struct KeyringState {
    busy: bool,
    /// None = da leggere.
    status: Option<secrets::Status>,
    error: Option<String>,
}

/// Risposte di Home Assistant, lette in background.
#[derive(Default)]
struct HaState {
    busy: bool,
    /// Esito dell'ultima prova di collegamento.
    check: Option<Result<String, String>>,
    /// Entità come schede dello store; None = non ancora lette.
    plugins: Option<Vec<Plugin>>,
    error: Option<String>,
}

/// Audio e brano letti in background per l'anteprima della pagina audio.
#[derive(Default)]
struct MixerLive {
    /// Tutti i canali, senza i filtri della pagina (servono all'elenco delle app).
    channels: Vec<Channel>,
    np: Option<NowPlaying>,
    art_url: String,
    art: Option<Vec<Rgb>>,
    version: u64,
}

impl Editor {
    fn new(cc: &eframe::CreationContext<'_>, cfg_path: PathBuf) -> Result<Self> {
        let mut cfg = config::load(&cfg_path)?;
        let fonts = Fonts::load(&cfg.layout.icon_font, &cfg.layout.text_font)?;
        let scheme_path = config::scheme_path();
        let theme = theme::for_config(&cfg.theme, &scheme_path);
        let intro_done = cfg.intro_done;
        pad_pages(&mut cfg);

        let mut defs = egui::FontDefinitions::default();
        defs.font_data.insert("rubik".into(), Arc::new(FontData::from_static(fonts.text_data)));
        defs.families.entry(FontFamily::Proportional).or_default().insert(0, "rubik".into());
        cc.egui_ctx.set_fonts(defs);
        apply_theme(&cc.egui_ctx, &theme);

        let (catalog, catalog_errors) = presets::catalog(&presets::plugins_dir(&cfg_path));
        Ok(Self {
            catalog,
            catalog_errors,
            store_search: String::new(),
            store_cat: None,
            store_page: 0,
            ha_token_input: String::new(),
            ha_token_saved: None,
            ha: Arc::default(),
            keyring: Arc::default(),
            bambu_code_input: String::new(),
            bambu_code_saved: None,
            printer_check: Arc::default(),
            disk_mtime: mtime(&cfg_path),
            cfg_path,
            saved: cfg.clone(),
            cfg,
            scheme_mtime: mtime(&scheme_path),
            scheme_path,
            theme,
            icon_names: icons::ligature_names(fonts.icon_data),
            fonts,
            apps: presets::desktop_apps(),
            // Nei test la guida resta chiusa, tranne in quello che la prova.
            intro: (!intro_done && !cfg!(test)).then_some(0),
            intro_pages: if intro_done { 0 } else { 1 },
            intro_check: None,
            tab: Tab::Keys,
            page: 0,
            selected: Some(0),
            drag_from: None,
            clipboard: None,
            picker: None,
            icon_search: String::new(),
            app_search: String::new(),
            auto_new: String::new(),
            slot_rects: Vec::new(),
            clock: bar::clock(),
            test_result: Arc::new(Mutex::new(None)),
            autosave: true,
            last_edit: None,
            next_file_check: Instant::now(),
            status: None,
            textures: HashMap::new(),
            used: HashSet::new(),
            live: Arc::default(),
            live_busy: Arc::default(),
            live_next: Instant::now(),
        })
    }

    fn dirty(&self) -> bool {
        self.cfg != self.saved
    }

    fn edited(&mut self) {
        self.last_edit = Some(Instant::now());
    }

    fn set_status(&mut self, msg: impl Into<String>, error: bool) {
        self.status = Some((msg.into(), error, Instant::now()));
    }

    fn mixer_page(&self) -> Option<&MixerConfig> {
        self.cfg.pages.get(self.page).filter(|p| p.kind == PageKind::Mixer).map(|p| &p.mixer)
    }

    /// Aggiorna in background volumi e brano mentre è aperta una pagina audio.
    fn poll_live(&mut self) {
        let widgets = self.cfg.pages.get(self.page).is_some_and(|p| p.keys.iter().any(|k| k.widget.is_some()));
        if (self.mixer_page().is_none() && !widgets) || Instant::now() < self.live_next || self.live_busy.swap(true, Ordering::SeqCst) {
            return;
        }
        self.live_next = Instant::now() + Duration::from_millis(1500);
        let (live, busy) = (self.live.clone(), self.live_busy.clone());
        std::thread::spawn(move || {
            let all = MixerConfig { player: true, master: true, mic: true, ..MixerConfig::default() };
            let channels = audio::snapshot(&all);
            let np = media::query(media::caelestia_player().as_deref());
            let url = np.as_ref().map(|n| n.art_url.clone()).unwrap_or_default();
            let old_url = live.lock().unwrap().art_url.clone();
            let art = (url != old_url).then(|| media::load_art(&url, mixer::ART));
            let mut l = live.lock().unwrap();
            if channels != l.channels || np != l.np || art.is_some() {
                l.version += 1;
            }
            l.channels = channels;
            l.np = np;
            if let Some(a) = art {
                l.art = a;
                l.art_url = url;
            }
            busy.store(false, Ordering::SeqCst);
        });
    }

    /// Barra di stato con valori d'esempio per la batteria.
    fn draw_bar(&self, c: &mut Canvas) {
        let page = self.cfg.pages.get(self.page).map(|p| p.name.as_str()).unwrap_or_default();
        let link = if self.cfg.wifi.enabled() { device::Link::Wifi } else { device::Link::Usb };
        let info = bar::Info { page, clock: &self.clock, battery: Some(80), charging: false, link: Some(link) };
        bar::draw(c, &self.fonts, &self.theme, &self.cfg.bar, config::bar_height(&self.cfg), &info);
    }

    fn bar_texture(&mut self, ctx: &egui::Context) -> Option<TextureHandle> {
        let h = config::bar_height(&self.cfg);
        if h == 0 {
            return None;
        }
        let (w, _) = device::screen_size(self.cfg.device.rotation);
        let page = self.cfg.pages.get(self.page).map(|p| p.name.clone()).unwrap_or_default();
        let id = format!("bar:{:?}:{page}:{}:{}", self.cfg.bar, self.clock, self.cfg.wifi.enabled());
        self.used.insert(id.clone());
        if let Some(t) = self.textures.get(&id) {
            return Some(t.clone());
        }
        let mut c = Canvas::new(w, h);
        self.draw_bar(&mut c);
        let img = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &c.rgba());
        let tex = ctx.load_texture(id.clone(), img, TextureOptions::LINEAR);
        self.textures.insert(id, tex.clone());
        Some(tex)
    }

    /// Anteprima della pagina audio disegnata con lo stesso renderer del deck.
    fn mixer_texture(&mut self, ctx: &egui::Context, cfg: &MixerConfig) -> TextureHandle {
        let live = self.live.clone();
        let live = live.lock().unwrap();
        let id = format!("mixer:{}:{cfg:?}:{:?}:{}:{}", live.version, self.cfg.bar, self.clock, self.cfg.pages[self.page].name);
        self.used.insert(id.clone());
        if let Some(t) = self.textures.get(&id) {
            return t.clone();
        }
        let (w, h) = device::screen_size(self.cfg.device.rotation);
        let channels: Vec<Channel> = live.channels.iter().filter(|c| audio::shown(c, cfg)).cloned().collect();
        let view = mixer::View {
            channels: &channels,
            np: live.np.as_ref(),
            art: live.art.as_deref(),
            scroll: 0,
            dragging: None,
            pressed: None,
        };
        let mut c = Canvas::new(w, h);
        mixer::draw_all(&mut c, &self.fonts, &self.theme, &mixer::Layout::new(cfg, w, h, config::bar_height(&self.cfg)), &view);
        self.draw_bar(&mut c);
        let img = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &c.rgba());
        let tex = ctx.load_texture(id.clone(), img, TextureOptions::LINEAR);
        self.textures.insert(id, tex.clone());
        tex
    }

    /// Anteprima dal vivo di una parte audio, grande `w`×`h` pixel del deck.
    fn widget_texture(&mut self, ctx: &egui::Context, key: &Key, w: u32, h: u32) -> TextureHandle {
        let live = self.live.clone();
        let live = live.lock().unwrap();
        let id = format!("widget:{}:{}:{w}x{h}:{key:?}", live.version, self.clock);
        self.used.insert(id.clone());
        if let Some(t) = self.textures.get(&id) {
            return t.clone();
        }
        let mut c = Canvas::new(w, h);
        let r = mixer::Rect { x: 0, y: 0, w, h };
        let radius = self.cfg.layout.radius;
        match key.widget {
            Some(Widget::Player) => widgets::draw_player(&mut c, &self.fonts, &self.theme, r, radius, live.np.as_ref(), live.art.as_deref(), None),
            Some(Widget::Clock) => widgets::draw_clock(&mut c, &self.fonts, &self.theme, r, radius, &self.clock, &bar::date()),
            Some(Widget::ClaudeLimits) => widgets::draw_limits(&mut c, &self.fonts, &self.theme, r, radius, crate::claude::load().as_ref(), crate::claude::now()),
            Some(Widget::Printer) => widgets::draw_printer(&mut c, &self.fonts, &self.theme, r, radius, Some(&printer::sample())),
            _ => {
                let ch = widgets::find(&live.channels, &key.target).map(|j| &live.channels[j]);
                widgets::draw_volume(&mut c, &self.fonts, &self.theme, r, radius, ch, &key.target, false);
            }
        }
        let img = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &c.rgba());
        let tex = ctx.load_texture(id.clone(), img, TextureOptions::LINEAR);
        self.textures.insert(id, tex.clone());
        tex
    }

    fn slots(&self) -> usize {
        (self.cfg.layout.cols.max(1) * self.cfg.layout.rows.max(1)) as usize
    }

    fn keys_mut(&mut self) -> &mut Vec<Key> {
        &mut self.cfg.pages[self.page].keys
    }

    fn save(&mut self) {
        // La password del Wi-Fi non va nel file: prima il portachiavi, poi la config.
        if self.cfg.wifi.password != self.saved.wifi.password {
            if let Err(e) = secrets::set(secrets::WIFI_PASSWORD, &self.cfg.wifi.password) {
                self.set_status(tr!("Password del Wi-Fi non salvata: {}", format!("{e:#}")), true);
                return;
            }
        }
        match config::save(&self.cfg_path, &self.cfg) {
            Ok(()) => {
                self.saved = self.cfg.clone();
                self.disk_mtime = mtime(&self.cfg_path);
                self.last_edit = None;
                self.set_status(tr!("Salvato: il deck si aggiorna da solo"), false);
            }
            Err(e) => self.set_status(tr!("Errore nel salvataggio: {}", format!("{e:#}")), true),
        }
    }

    /// Ricalcola i colori (dopo un cambio di tema nella config o dello schema di Caelestia).
    fn refresh_theme(&mut self, ctx: &egui::Context) {
        self.theme = theme::for_config(&self.cfg.theme, &self.scheme_path);
        apply_theme(ctx, &self.theme);
        self.textures.clear();
    }

    /// Ricarica tema e config se sono cambiati fuori dall'editor.
    fn poll_files(&mut self, ctx: &egui::Context) {
        if Instant::now() < self.next_file_check {
            return;
        }
        self.next_file_check = Instant::now() + Duration::from_secs(1);
        self.clock = bar::clock();
        let m = mtime(&self.scheme_path);
        if m != self.scheme_mtime {
            self.scheme_mtime = m;
            self.refresh_theme(ctx);
        }
        let m = mtime(&self.cfg_path);
        if m != self.disk_mtime {
            self.disk_mtime = m;
            if self.dirty() {
                self.set_status(tr!("La config è cambiata anche sul disco: al salvataggio vince l'editor"), true);
            } else if let Ok(mut cfg) = config::load(&self.cfg_path) {
                pad_pages(&mut cfg);
                self.saved = cfg.clone();
                self.cfg = cfg;
                self.page = self.page.min(self.cfg.pages.len() - 1);
                self.set_status(tr!("Config ricaricata dal disco"), false);
            }
        }
    }

    fn texture(&mut self, ctx: &egui::Context, id: String, make: impl FnOnce(&Self) -> (u32, Vec<u8>)) -> TextureHandle {
        self.used.insert(id.clone());
        if let Some(t) = self.textures.get(&id) {
            return t.clone();
        }
        let (px, rgba) = make(self);
        let tex = ctx.load_texture(
            id.clone(),
            ColorImage::from_rgba_unmultiplied([px as usize, px as usize], &rgba),
            TextureOptions::LINEAR,
        );
        self.textures.insert(id, tex.clone());
        tex
    }

    /// Anteprima del tasto disegnata con lo stesso renderer del deck.
    fn key_texture(&mut self, ctx: &egui::Context, key: &Key, active: bool, px: u32) -> TextureHandle {
        let px = px.max(8);
        let id = format!("key|{px}|{active}|{key:?}");
        let key = key.clone();
        self.texture(ctx, id, move |s| {
            let grid = Grid::new(&s.cfg, device::screen_size(s.cfg.device.rotation).0, device::screen_size(s.cfg.device.rotation).1);
            let radius = s.cfg.layout.radius * px as f32 / grid.size.max(1) as f32;
            let mut c = Canvas::new(px, px);
            c.fill(s.theme.background);
            let icon = if active { key.icon_active.as_ref().or(key.icon.as_ref()) } else { key.icon.as_ref() };
            // Nell'anteprima la batteria è un valore d'esempio.
            let icon = icon.map(|i| app::expand(i, Some(80), false, true));
            let label = key.label.as_ref().map(|l| app::expand(l, Some(80), false, false));
            let visual = KeyVisual {
                icon: icon.as_deref(),
                label: label.as_deref(),
                style: if active { Style::Active } else { Style::Normal },
            };
            render::draw_key(&mut c, &s.fonts, &s.theme, 0, 0, px, radius, &visual);
            (px, c.rgba())
        })
    }

    fn icon_texture(&mut self, ctx: &egui::Context, name: &str, px: u32) -> Option<TextureHandle> {
        if !self.fonts.has_icon(name) {
            return None;
        }
        let name = name.to_string();
        Some(self.texture(ctx, format!("icon|{px}|{name}"), move |s| (px, s.fonts.icon_mask(&name, px).unwrap_or_default())))
    }

    fn icon_image(&mut self, ctx: &egui::Context, name: &str, size: f32) -> Option<egui::Image<'static>> {
        let px = (size * ctx.pixels_per_point()).round() as u32;
        let tex = self.icon_texture(ctx, name, px)?;
        Some(egui::Image::from_texture(SizedTexture::new(tex.id(), vec2(size, size))).tint(c32(self.theme.primary)))
    }

    // ---------------------------------------------------------------- toolbar

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("CYD Deck").size(22.0).strong().color(c32(self.theme.primary)));
            ui.add_space(12.0);
            ui.selectable_value(&mut self.tab, Tab::Keys, RichText::new(tr!("Tasti")).size(15.0));
            ui.selectable_value(&mut self.tab, Tab::Store, RichText::new(tr!("Store")).size(15.0));
            ui.selectable_value(&mut self.tab, Tab::Settings, RichText::new(tr!("Impostazioni")).size(15.0));

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let dirty = self.dirty();
                let save = egui::Button::new(RichText::new(tr!("Salva")).color(c32(self.theme.on_primary)))
                    .fill(if dirty { c32(self.theme.primary) } else { c32(self.theme.surface_container_high) });
                if ui.add_enabled(dirty, save).on_hover_text("Ctrl+S").clicked() {
                    self.save();
                }
                if ui.add_enabled(dirty, egui::Button::new(tr!("Annulla modifiche"))).clicked() {
                    self.cfg = self.saved.clone();
                    self.page = self.page.min(self.cfg.pages.len() - 1);
                    self.set_status(tr!("Modifiche annullate"), false);
                }
                ui.checkbox(&mut self.autosave, tr!("Applica subito al deck"));
                if let Some((msg, err, at)) = &self.status {
                    if at.elapsed() < Duration::from_secs(6) {
                        let col = if *err { self.theme.error } else { self.theme.on_surface_variant };
                        ui.label(RichText::new(msg).color(c32(col)));
                    }
                } else if dirty {
                    ui.label(RichText::new(tr!("Modifiche non salvate")).color(c32(self.theme.on_surface_variant)));
                }
            });
        });
        ui.add_space(6.0);
    }

    fn page_tabs(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            let n = self.cfg.pages.len();
            let mut action: Option<(usize, i32)> = None;
            for i in 0..n {
                let name = if self.cfg.pages[i].name.is_empty() { tr!("Pagina {}", i + 1) } else { self.cfg.pages[i].name.clone() };
                let label = if self.cfg.pages[i].hidden { RichText::new(tr!("{} · nascosta", name)).size(15.0).italics() } else { RichText::new(name).size(15.0) };
                let r = ui.selectable_label(i == self.page, label);
                if r.clicked() {
                    self.page = i;
                    self.selected = None;
                }
                r.context_menu(|ui| {
                    if ui.add_enabled(i > 0, egui::Button::new(tr!("Sposta a sinistra"))).clicked() {
                        action = Some((i, -1));
                        ui.close();
                    }
                    if ui.add_enabled(i + 1 < n, egui::Button::new(tr!("Sposta a destra"))).clicked() {
                        action = Some((i, 1));
                        ui.close();
                    }
                    if ui.add_enabled(n > 1, egui::Button::new(tr!("Elimina pagina"))).clicked() {
                        action = Some((i, 0));
                        ui.close();
                    }
                });
            }
            let mut new_page = None;
            let mut claude_page = false;
            menu(ui, tr!("+ Pagina"), |ui| {
                if ui.button(tr!("Pagina di tasti")).clicked() {
                    new_page = Some(PageKind::Keys);
                    ui.close();
                }
                if ui.button(tr!("Pagina audio (volumi e musica)")).clicked() {
                    new_page = Some(PageKind::Mixer);
                    ui.close();
                }
                if ui.button(tr!("Pagina limiti Claude")).clicked() {
                    claude_page = true;
                    ui.close();
                }
            });
            if claude_page {
                // Limiti su tutta la larghezza e, nell'ultima riga, un tasto che apre Claude Code.
                let (cols, rows) = (self.cfg.layout.cols.max(1), self.cfg.layout.rows.max(1));
                let mut keys = vec![Key::default(); self.slots()];
                keys[0] = Key { widget: Some(Widget::ClaudeLimits), w: cols, h: (rows - 1).max(1), ..Key::default() };
                if rows > 1 {
                    keys[((rows - 1) * cols) as usize] = presets::GROUPS.iter().flat_map(|g| g.presets).find(|p| p.name == "Claude Code").map(Preset::to_key).unwrap_or_default();
                }
                let name = if self.cfg.pages.iter().any(|p| p.name == "Claude") { format!("Claude {}", n + 1) } else { tr!("Claude").into() };
                self.cfg.pages.push(Page { name, keys, ..Page::default() });
                self.page = n;
                self.selected = Some(0);
                self.edited();
            }
            if let Some(kind) = new_page {
                let base = if kind == PageKind::Mixer { "Audio" } else { tr!("Pagina") };
                let name = (1..)
                    .map(|i| if i == 1 && kind == PageKind::Mixer { base.to_string() } else { format!("{base} {}", n + i) })
                    .find(|c| !self.cfg.pages.iter().any(|p| &p.name == c))
                    .unwrap_or_default();
                let keys = if kind == PageKind::Keys { vec![Key::default(); self.slots()] } else { Vec::new() };
                self.cfg.pages.push(Page { name, kind, keys, ..Page::default() });
                self.page = n;
                self.selected = (kind == PageKind::Keys).then_some(0);
                self.edited();
            }
            match action {
                Some((i, 0)) => {
                    self.cfg.pages.remove(i);
                    self.page = self.page.min(self.cfg.pages.len() - 1);
                    self.selected = None;
                    self.edited();
                }
                Some((i, d)) => {
                    let j = (i as i32 + d) as usize;
                    self.cfg.pages.swap(i, j);
                    if self.page == i {
                        self.page = j;
                    } else if self.page == j {
                        self.page = i;
                    }
                    self.edited();
                }
                None => {}
            }
        });
    }

    // ---------------------------------------------------------------- deck

    /// App che aprono la pagina quando vanno in primo piano.
    fn auto_apps(&mut self, ui: &mut egui::Ui) {
        let t = self.theme.clone();
        let mut auto = self.cfg.pages[self.page].auto.clone();
        ui.horizontal_wrapped(|ui| {
            ui.label(tr!("Si apre da sola con")).on_hover_text(
                tr!("Quando una di queste app va in primo piano il deck passa a questa pagina, \
                 e torna alla precedente quando la lasci. Basta una parte del nome (classe della finestra)."),
            );
            let mut remove = None;
            for (i, a) in auto.iter().enumerate() {
                if ui.button(format!("{a}  ×")).on_hover_text(tr!("Togli")).clicked() {
                    remove = Some(i);
                }
            }
            if let Some(i) = remove {
                auto.remove(i);
            }
            let field = ui.add(TextEdit::singleline(&mut self.auto_new).desired_width(120.0).hint_text(tr!("es. spotify")));
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui.button(tr!("Aggiungi")).clicked() || enter) && !self.auto_new.trim().is_empty() {
                auto.push(self.auto_new.trim().to_string());
                self.auto_new.clear();
            }
            menu(ui, tr!("App aperte"), |ui| {
                let classes = open_window_classes();
                if classes.is_empty() {
                    ui.label(RichText::new(tr!("Nessuna finestra aperta")).color(c32(t.on_surface_variant)));
                }
                for c in classes {
                    if ui.button(&c).clicked() {
                        if !auto.iter().any(|a| a.eq_ignore_ascii_case(&c)) {
                            auto.push(c);
                        }
                        ui.close();
                    }
                }
            });
        });
        let mut hidden = self.cfg.pages[self.page].hidden;
        ui.horizontal(|ui| {
            ui.checkbox(&mut hidden, tr!("Nascosta: si apre solo con queste app")).on_hover_text(
                tr!("Lo swipe sul deck la salta: compare quando apri una delle app qui sopra (o con un tasto \"Vai a una pagina\") \
                 e sparisce quando la lasci."),
            );
            if hidden && auto.is_empty() {
                ui.label(RichText::new(tr!("aggiungi almeno un'app")).color(c32(t.error)));
            }
        });
        if auto != self.cfg.pages[self.page].auto || hidden != self.cfg.pages[self.page].hidden {
            self.cfg.pages[self.page].auto = auto;
            self.cfg.pages[self.page].hidden = hidden;
            self.edited();
        }
    }

    fn deck_view(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.page_tabs(ui);
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let label = ui.label(tr!("Nome pagina"));
            let old = self.cfg.pages[self.page].name.clone();
            let mut name = old.clone();
            if ui.add(TextEdit::singleline(&mut name).desired_width(220.0)).labelled_by(label.id).changed() {
                // Aggiorna anche i tasti che puntano a questa pagina.
                for p in &mut self.cfg.pages {
                    for k in &mut p.keys {
                        if k.action.as_deref() == Some(&format!("page:{old}")) {
                            k.action = Some(format!("page:{name}"));
                        }
                    }
                }
                self.cfg.pages[self.page].name = name;
                self.edited();
            }
        });
        self.auto_apps(ui);

        let (w, h) = device::screen_size(self.cfg.device.rotation);
        let grid = Grid::new(&self.cfg, w, h);
        let area = ui.available_rect_before_wrap();
        let bezel = 18.0;
        let k = ((area.width() - 2.0 * bezel - 20.0) / w as f32)
            .min((area.height() - 2.0 * bezel - 40.0) / h as f32)
            .clamp(0.6, 3.0);
        let outer = Rect::from_center_size(
            area.center() - vec2(0.0, 12.0),
            vec2(w as f32 * k + 2.0 * bezel, h as f32 * k + 2.0 * bezel),
        );
        ui.allocate_rect(area, Sense::hover());
        let painter = ui.painter().clone();
        painter.rect_filled(outer, CornerRadius::same(26), Color32::from_gray(14));
        let screen = outer.shrink(bezel);
        painter.rect_filled(screen, CornerRadius::same(3), c32(self.theme.background));
        if let Some(tex) = self.bar_texture(&ctx) {
            let bh = config::bar_height(&self.cfg) as f32 * k;
            let r = Rect::from_min_size(screen.min, vec2(screen.width(), bh));
            painter.image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }

        if let Some(cfg) = self.mixer_page().cloned() {
            let tex = self.mixer_texture(&ctx, &cfg);
            painter.image(tex.id(), screen, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            painter.text(
                pos2(outer.center().x, outer.max.y + 18.0),
                Align2::CENTER_CENTER,
                tr!("Anteprima dal vivo  ·  le opzioni della pagina sono a destra"),
                FontId::proportional(13.0),
                c32(self.theme.on_surface_variant),
            );
            return;
        }

        let ppp = ctx.pixels_per_point();
        let tile = grid.size as f32 * k;
        let radius = (self.cfg.layout.radius * k) as u8;
        // Le parti audio grandi coprono più posti: i tasti sotto non si vedono.
        let owners = config::owners(&self.cfg.pages[self.page].keys, grid.cols, grid.rows);
        let areas: Vec<mixer::Rect> = (0..grid.slots())
            .map(|i| {
                let (w, h) = config::cells(&owners, i, grid.cols);
                grid.area(i, w, h)
            })
            .collect();
        let slot_rect = |i: usize| {
            let a = areas[i];
            Rect::from_min_size(screen.min + vec2(a.x as f32 * k, a.y as f32 * k), vec2(a.w as f32 * k, a.h as f32 * k))
        };
        let pointer = ctx.pointer_interact_pos();
        let hovered_slot = pointer.and_then(|p| (0..grid.slots()).find(|&i| owners[i] == i && slot_rect(i).contains(p)));
        self.slot_rects = (0..grid.slots()).map(slot_rect).collect();
        if let Some(sel) = self.selected.filter(|&s| s < owners.len() && owners[s] != s) {
            self.selected = Some(owners[sel]);
        }

        for i in (0..grid.slots()).filter(|&i| owners[i] == i) {
            let r = slot_rect(i);
            let resp = ui.interact(r, Id::unique(("cyd-slot", self.page, i)), Sense::click_and_drag());
            let key = self.cfg.pages[self.page].keys.get(i).cloned().unwrap_or_default();
            let empty = key == Key::default();

            if self.drag_from == Some(i) {
                painter.rect_stroke(r.shrink(1.0), CornerRadius::same(radius), Stroke::new(1.5, c32(self.theme.outline_variant)), StrokeKind::Inside);
            } else if key.widget.is_some() {
                let tex = self.widget_texture(&ctx, &key, areas[i].w, areas[i].h);
                painter.image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            } else if !empty {
                let tex = self.key_texture(&ctx, &key, false, (tile * ppp).round() as u32);
                painter.image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            } else {
                painter.rect_stroke(r.shrink(1.0), CornerRadius::same(radius), Stroke::new(1.5, c32(self.theme.outline_variant)), StrokeKind::Inside);
                painter.text(r.center(), Align2::CENTER_CENTER, "+", FontId::proportional(tile * 0.3), c32(self.theme.outline_variant));
            }

            let target = self.drag_from.is_some() && hovered_slot == Some(i) && self.drag_from != Some(i);
            if self.selected == Some(i) {
                painter.rect_stroke(r.expand(3.0), CornerRadius::same(radius + 3), Stroke::new(2.5, c32(self.theme.primary)), StrokeKind::Outside);
            } else if target || (resp.hovered() && self.drag_from.is_none()) {
                painter.rect_stroke(r.expand(3.0), CornerRadius::same(radius + 3), Stroke::new(1.5, c32(self.theme.outline)), StrokeKind::Outside);
            }

            if resp.clicked() {
                self.selected = Some(i);
            }
            if resp.drag_started() && !empty {
                self.drag_from = Some(i);
                self.selected = Some(i);
            }
            resp.context_menu(|ui| {
                if ui.add_enabled(!empty, egui::Button::new(tr!("Copia"))).clicked() {
                    self.clipboard = Some(key.clone());
                    ui.close();
                }
                if ui.add_enabled(self.clipboard.is_some(), egui::Button::new(tr!("Incolla"))).clicked() {
                    if let Some(c) = self.clipboard.clone() {
                        self.set_key(i, c);
                        self.selected = Some(i);
                    }
                    ui.close();
                }
                if ui.add_enabled(!empty && key.exec.is_some(), egui::Button::new(tr!("Prova comando"))).clicked() {
                    if let Some(cmd) = &key.exec {
                        app::spawn_shell(cmd);
                    }
                    ui.close();
                }
                if ui.add_enabled(!empty, egui::Button::new(tr!("Svuota"))).clicked() {
                    self.set_key(i, Key::default());
                    ui.close();
                }
            });
        }

        // Trascinamento: il tasto segue il puntatore e al rilascio si scambia col tasto sotto.
        if let Some(from) = self.drag_from {
            let key = self.cfg.pages[self.page].keys.get(from).cloned().unwrap_or_default();
            if let Some(p) = pointer {
                let (tex, size) = if key.widget.is_some() {
                    let a = areas[from];
                    (self.widget_texture(&ctx, &key, a.w, a.h), vec2(a.w as f32 * k, a.h as f32 * k))
                } else {
                    (self.key_texture(&ctx, &key, false, (tile * ppp).round() as u32), vec2(tile, tile))
                };
                let lp = ctx.layer_painter(LayerId::new(Order::Tooltip, Id::unique("cyd-drag-key")));
                let r = Rect::from_center_size(p, size * 1.06);
                lp.image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::from_white_alpha(230));
            }
            if ctx.input(|i| i.pointer.any_released()) {
                if let Some(to) = hovered_slot.filter(|&t| t != from) {
                    self.keys_mut().swap(from, to);
                    self.selected = Some(to);
                    self.edited();
                }
                self.drag_from = None;
            }
        }

        painter.text(
            pos2(outer.center().x, outer.max.y + 18.0),
            Align2::CENTER_CENTER,
            tr!("Clic per modificare  ·  trascina per spostare  ·  tasto destro per copia/incolla"),
            FontId::proportional(13.0),
            c32(self.theme.on_surface_variant),
        );
    }

    fn mixer_inspector(&mut self, ui: &mut egui::Ui) {
        let t = self.theme.clone();
        let mut m = self.cfg.pages[self.page].mixer.clone();
        let apps: Vec<(String, &'static str)> = {
            let live = self.live.lock().unwrap();
            live.channels.iter().filter(|c| matches!(c.target, Target::Nodes(_))).map(|c| (c.name.clone(), c.icon)).collect()
        };
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.label(RichText::new(tr!("Pagina audio")).size(18.0).strong());
            ui.add_space(8.0);
            section(ui, &t, tr!("Cosa mostrare"));
            ui.checkbox(&mut m.player, tr!("Lettore musicale"))
                .on_hover_text(tr!("Il player attivo di Caelestia (lo stesso della dashboard), con copertina e controlli"));
            ui.checkbox(&mut m.master, tr!("Volume generale (uscita predefinita)"));
            ui.checkbox(&mut m.mic, tr!("Microfono"));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(tr!("Cursori"));
                ui.selectable_value(&mut m.vertical, true, tr!("Verticali"));
                ui.selectable_value(&mut m.vertical, false, tr!("Orizzontali"));
            });
            ui.add_space(10.0);

            section(ui, &t, tr!("App"));
            // App che suonano adesso, più quelle nascoste che al momento non suonano.
            let mut names: Vec<(String, &str)> = apps.clone();
            for h in &m.hide {
                if !names.iter().any(|(n, _)| n.eq_ignore_ascii_case(h)) {
                    names.push((h.clone(), audio::app_icon(h, "")));
                }
            }
            if names.is_empty() {
                ui.label(
                    RichText::new(tr!("Nessuna app sta riproducendo audio adesso. Ogni app compare da sola, sul deck e qui, appena emette suoni."))
                        .color(c32(t.on_surface_variant)),
                );
            }
            for (name, icon) in names {
                let hidden = m.hide.iter().any(|h| h.eq_ignore_ascii_case(&name));
                let mut shown = !hidden;
                ui.horizontal(|ui| {
                    if let Some(tex) = self.icon_texture(ui.ctx(), icon, 20) {
                        ui.add(egui::Image::from_texture(SizedTexture::new(tex.id(), vec2(20.0, 20.0))).tint(c32(t.primary)).alt_text(icon));
                    }
                    ui.checkbox(&mut shown, &name);
                });
                if shown == hidden {
                    if shown {
                        m.hide.retain(|h| !h.eq_ignore_ascii_case(&name));
                    } else {
                        m.hide.push(name.clone());
                    }
                }
            }
            ui.add_space(10.0);

            section(ui, &t, tr!("Sul deck"));
            let lines: &[&str] = if m.vertical {
                &[
                    tr!("Trascina un cursore in su o in giù per il volume"),
                    tr!("Tocca il nome sotto il cursore per il muto"),
                    tr!("Scorri di lato sui cursori se le app sono più di 5"),
                    tr!("Tocca la copertina per aprire il workspace musica di Caelestia"),
                    tr!("Scorri di lato sulla scheda del lettore per cambiare pagina"),
                ]
            } else {
                &[
                    tr!("Trascina un cursore per cambiare il volume"),
                    tr!("Tocca icona o nome per il muto"),
                    tr!("Scorri in verticale se le app sono tante"),
                    tr!("Tocca la copertina per aprire il workspace musica di Caelestia"),
                    tr!("Scorri in orizzontale sulla scheda del lettore per cambiare pagina"),
                ]
            };
            for line in lines {
                ui.label(RichText::new(format!("·  {line}")).color(c32(t.on_surface_variant)));
            }
        });
        if m != self.cfg.pages[self.page].mixer {
            self.cfg.pages[self.page].mixer = m;
            self.edited();
        }
    }

    fn set_key(&mut self, i: usize, key: Key) {
        let keys = self.keys_mut();
        if keys.len() <= i {
            keys.resize(i + 1, Key::default());
        }
        keys[i] = key;
        self.edited();
    }

    // ---------------------------------------------------------------- inspector

    fn inspector(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.add_space(8.0);
        if self.mixer_page().is_some() {
            self.mixer_inspector(ui);
            return;
        }
        let Some(i) = self.selected.filter(|&i| i < self.slots()) else {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| ui.label(RichText::new(tr!("Seleziona un tasto da modificare")).color(c32(self.theme.on_surface_variant))));
            return;
        };
        let original = self.cfg.pages[self.page].keys.get(i).cloned().unwrap_or_default();
        let mut k = original.clone();

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr!("Tasto {}", i + 1)).size(18.0).strong());
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let mut w = k.widget;
                ui.selectable_value(&mut w, None, tr!("Tasto"));
                ui.selectable_value(&mut w, Some(Widget::Volume), tr!("Cursore volume"));
                ui.selectable_value(&mut w, Some(Widget::Player), tr!("Lettore musicale"));
                ui.selectable_value(&mut w, Some(Widget::Clock), tr!("Orologio"));
                ui.selectable_value(&mut w, Some(Widget::ClaudeLimits), tr!("Limiti Claude"));
                ui.selectable_value(&mut w, Some(Widget::Printer), tr!("Stampante 3D"));
                if w != k.widget {
                    // Posti liberi fino al bordo della griglia, a destra e in basso.
                    let cols = self.cfg.layout.cols.max(1);
                    let (cols, rows) = (cols - i as u32 % cols, self.cfg.layout.rows.max(1) - (i as u32 / cols).min(self.cfg.layout.rows.max(1) - 1));
                    k = match w {
                        None => Key::default(),
                        // Di partenza: cursore fino in fondo alla griglia, lettore fino al bordo destro.
                        Some(Widget::Volume) => Key { widget: w, w: 1, h: rows, ..Key::default() },
                        Some(Widget::Player) => Key { widget: w, w: cols, h: 1, ..Key::default() },
                        Some(Widget::Clock) => Key { widget: w, w: cols.min(2), h: 1, ..Key::default() },
                        Some(Widget::ClaudeLimits) => Key { widget: w, w: cols, h: rows.min(2), ..Key::default() },
                        Some(Widget::Printer) => Key { widget: w, w: cols, h: 1, ..Key::default() },
                    };
                }
            });
            ui.add_space(8.0);
            if k.widget.is_some() {
                self.widget_options(ui, &mut k);
            }

            // Anteprime: normale e, se c'è uno stato, attivo.
            if k.widget.is_none() && k != Key::default() {
                ui.horizontal(|ui| {
                    let px = (96.0 * ctx.pixels_per_point()) as u32;
                    let tex = self.key_texture(&ctx, &k, false, px);
                    ui.add(egui::Image::from_texture(SizedTexture::new(tex.id(), vec2(96.0, 96.0))));
                    if k.state.is_some() {
                        let tex = self.key_texture(&ctx, &k, true, px);
                        ui.add(egui::Image::from_texture(SizedTexture::new(tex.id(), vec2(96.0, 96.0))))
                            .on_hover_text(tr!("Come appare quando lo stato è attivo"));
                    }
                });
                ui.add_space(8.0);
            }

            if k.widget.is_none() {
                section(ui, &self.theme, tr!("Aspetto"));
                ui.label(tr!("Icona"));
                self.icon_row(ui, &ctx, &mut k.icon, IconTarget::Normal);
                ui.add_space(4.0);
                ui.label(tr!("Etichetta"));
                opt_edit(ui, &mut k.label, tr!("Testo sotto l'icona"), false, 1);
                ui.add_space(10.0);

                section(ui, &self.theme, tr!("Azione"));
                let mut kind = ActionKind::of(&k);
                let label = ui.label(tr!("Cosa fa il tasto"));
                egui::ComboBox::from_id_salt("action-kind").width(240.0).selected_text(kind.label()).show_ui(ui, |ui| {
                    for a in ActionKind::ALL {
                        ui.selectable_value(&mut kind, a, a.label());
                    }
                })
                .response
                .labelled_by(label.id);
                if kind != ActionKind::of(&k) {
                    let old_exec = k.exec.take().filter(|e| !e.trim().is_empty());
                    k.steps.clear();
                    k.step_delay = 0;
                    k.action = match kind {
                        ActionKind::None => None,
                        ActionKind::Macro => {
                            // Il comando attuale diventa il primo passo.
                            k.steps.push(Step::Exec { exec: old_exec.unwrap_or_default(), workspace: None, background: false, wait: false });
                            None
                        }
                        ActionKind::Command => {
                            k.exec = Some(String::new());
                            None
                        }
                        ActionKind::Page => Some(format!("page:{}", self.cfg.pages[0].name)),
                        ActionKind::NextPage => Some("next-page".into()),
                        ActionKind::PrevPage => Some("prev-page".into()),
                        ActionKind::BacklightOff => Some("backlight-off".into()),
                        ActionKind::Reload => Some("reload".into()),
                    };
                }
                match kind {
                    ActionKind::Command => {
                        let mut exec = k.exec.clone().unwrap_or_default();
                        ui.add(TextEdit::multiline(&mut exec).code_editor().desired_rows(3).desired_width(f32::INFINITY).hint_text(tr!("es. foot -e btop")));
                        k.exec = Some(exec);
                        ui.horizontal(|ui| {
                            if ui.button(tr!("Prova")).on_hover_text(tr!("Esegue il comando adesso")).clicked() {
                                app::spawn_shell(k.exec.as_deref().unwrap_or_default());
                            }
                            self.preset_menu(ui, &mut k);
                        });
                    }
                    ActionKind::Macro => self.steps_editor(ui, &ctx, &mut k),
                    ActionKind::Page => {
                        let current = k.action.as_deref().and_then(|a| a.strip_prefix("page:")).unwrap_or_default().to_string();
                        let label = ui.label(tr!("Pagina da aprire"));
                        egui::ComboBox::from_id_salt("target-page").width(240.0).selected_text(current.clone()).show_ui(ui, |ui| {
                            for p in &self.cfg.pages {
                                if ui.selectable_label(p.name == current, &p.name).clicked() {
                                    k.action = Some(format!("page:{}", p.name));
                                }
                            }
                        })
                        .response
                        .labelled_by(label.id);
                    }
                    ActionKind::None => {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(tr!("Parti da un preset:")).color(c32(self.theme.on_surface_variant)));
                            self.preset_menu(ui, &mut k);
                        });
                    }
                    _ => {}
                }
                ui.add_space(10.0);

                egui::CollapsingHeader::new(RichText::new(tr!("Stato dinamico (avanzato)")).strong())
                    .default_open(k.state.is_some() || k.label_cmd.is_some())
                    .show(ui, |ui| {
                        ui.label(RichText::new(tr!("Comando di stato: se esce con codice 0 il tasto si illumina")).color(c32(self.theme.on_surface_variant)));
                        opt_edit(ui, &mut k.state, tr!("es. pgrep -x spotify"), true, 2);
                        ui.add_space(4.0);
                        ui.label(tr!("Icona quando è attivo"));
                        self.icon_row(ui, &ctx, &mut k.icon_active, IconTarget::Active);
                        ui.add_space(4.0);
                        ui.label(RichText::new(tr!("Comando etichetta: il suo output sostituisce l'etichetta")).color(c32(self.theme.on_surface_variant)));
                        opt_edit(ui, &mut k.label_cmd, tr!("es. date +%H:%M"), true, 2);
                        ui.add_space(4.0);
                        if ui.add_enabled(k.state.is_some() || k.label_cmd.is_some(), egui::Button::new(tr!("Prova stato"))).clicked() {
                            self.test_state(&k, &ctx);
                        }
                        if let Some(res) = self.test_result.lock().unwrap().clone() {
                            ui.label(RichText::new(res).monospace());
                        }
                    });
            }

            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if ui.button(tr!("Duplica")).on_hover_text(tr!("Copia nel primo spazio libero")).clicked() {
                    let slots = self.slots();
                    let keys = &mut self.cfg.pages[self.page].keys;
                    if let Some(j) = (0..slots).find(|&j| keys.get(j).is_none_or(|x| *x == Key::default())) {
                        if keys.len() <= j {
                            keys.resize(j + 1, Key::default());
                        }
                        keys[j] = k.clone();
                        self.selected = Some(j);
                        self.edited();
                    }
                }
                let clear = egui::Button::new(RichText::new(tr!("Svuota tasto")).color(c32(self.theme.error)));
                if ui.add(clear).clicked() {
                    k = Key::default();
                }
            });
        });

        if k != original && self.selected == Some(i) {
            self.set_key(i, k);
        }
    }

    /// Opzioni di una parte audio messa nella griglia.
    fn widget_options(&mut self, ui: &mut egui::Ui, k: &mut Key) {
        let t = self.theme.clone();
        if k.widget == Some(Widget::Volume) {
            section(ui, &t, tr!("Volume di"));
            let apps: Vec<String> = {
                let live = self.live.lock().unwrap();
                live.channels.iter().filter(|c| matches!(c.target, Target::Nodes(_))).map(|c| c.name.clone()).collect()
            };
            let label = ui.label(tr!("Cosa regola"));
            egui::ComboBox::from_id_salt("widget-target").width(240.0).selected_text(widgets::target_name(&k.target)).show_ui(ui, |ui| {
                ui.selectable_value(&mut k.target, String::new(), tr!("Generale (uscita predefinita)"));
                ui.selectable_value(&mut k.target, "@mic".to_string(), tr!("Microfono"));
                for a in &apps {
                    ui.selectable_value(&mut k.target, a.clone(), a);
                }
            })
            .response
            .labelled_by(label.id);
            ui.horizontal(|ui| {
                ui.label(tr!("oppure app"));
                let mut app = if k.target.starts_with('@') { String::new() } else { k.target.clone() };
                if ui.add(TextEdit::singleline(&mut app).desired_width(160.0).hint_text(tr!("es. Spotify"))).changed() {
                    k.target = app.trim().to_string();
                }
            });
            ui.label(
                RichText::new(tr!("Le app compaiono nell'elenco mentre suonano. Se l'app è chiusa il cursore resta grigio."))
                    .color(c32(t.on_surface_variant)),
            );
            ui.add_space(10.0);
        }

        section(ui, &t, tr!("Dimensioni"));
        let (cols, rows) = (self.cfg.layout.cols.max(1), self.cfg.layout.rows.max(1));
        let size_row = |ui: &mut egui::Ui, name: &str, v: &mut u32, max: u32| {
            ui.horizontal(|ui| {
                ui.add_sized([70.0, 20.0], egui::Label::new(name));
                for n in 1..=max {
                    if ui.selectable_label((*v).max(1) == n, n.to_string()).clicked() {
                        *v = n;
                    }
                }
                ui.label(RichText::new("tasti").color(c32(t.on_surface_variant)));
            });
        };
        size_row(ui, tr!("Larghezza"), &mut k.w, cols);
        size_row(ui, tr!("Altezza"), &mut k.h, rows);
        ui.label(
            RichText::new(tr!("Copre i tasti vicini (restano salvati e ricompaiono se la rimpicciolisci). Trascinala per spostarla."))
                .color(c32(t.on_surface_variant)),
        );
        ui.add_space(10.0);

        section(ui, &t, tr!("Sul deck"));
        let lines: &[&str] = if k.widget == Some(Widget::Volume) {
            &[tr!("Trascina lungo il cursore per il volume"), tr!("Tocca senza trascinare per il muto"), tr!("Più alto che largo = verticale, altrimenti orizzontale")]
        } else if k.widget == Some(Widget::ClaudeLimits) {
            &[
                tr!("Uso della sessione (5 ore) e della settimana, con il tempo all'azzeramento"),
                tr!("I dati arrivano da Claude Code e si aggiornano mentre lo usi"),
                tr!("Grande 2 righe mostra tutto; su una riga la sessione; su un tasto solo la percentuale"),
            ]
        } else if k.widget == Some(Widget::Printer) {
            &[
                tr!("Barra di avanzamento, percentuale, tempo rimanente, strato e temperature"),
                tr!("Su una riga: barra e dettagli; grande 2 righe: tutto più in grande; su un tasto solo la percentuale"),
                tr!("Qui sotto è un'anteprima d'esempio: sul deck arrivano i dati veri della stampante"),
            ]
        } else if k.widget == Some(Widget::Clock) {
            &[tr!("Ora e data, aggiornate ogni minuto"), tr!("Su un tasto solo la data compare se c'è spazio: allargalo a 2 per vederla bene")]
        } else {
            &[
                tr!("Tocca indietro, play/pausa e avanti per il lettore di Caelestia"),
                tr!("Tocca la copertina per il workspace musica"),
                tr!("Su un tasto solo: tocca per play/pausa"),
            ]
        };
        for line in lines {
            ui.label(RichText::new(format!("·  {line}")).color(c32(t.on_surface_variant)));
        }
        ui.add_space(4.0);
    }

    fn icon_row(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, icon: &mut Option<String>, target: IconTarget) {
        ui.horizontal(|ui| {
            let img = icon.as_deref().and_then(|n| self.icon_image(ctx, n, 28.0));
            let btn = match img {
                Some(img) => egui::Button::image(img),
                None => egui::Button::new("?"),
            };
            if ui.add(btn.min_size(vec2(44.0, 44.0))).on_hover_text(tr!("Scegli un'icona")).clicked() {
                self.picker = Some(target);
                self.icon_search.clear();
            }
            let mut name = icon.clone().unwrap_or_default();
            let r = ui.add(TextEdit::singleline(&mut name).hint_text(tr!("nome icona")).desired_width(170.0));
            if r.changed() {
                *icon = (!name.is_empty()).then_some(name.clone());
            }
            if !name.is_empty() && !self.fonts.has_icon(&name) {
                ui.label(RichText::new(tr!("non trovata")).color(c32(self.theme.error)));
            }
            if ui.button(tr!("Scegli…")).clicked() {
                self.picker = Some(target);
                self.icon_search.clear();
            }
        });
    }

    fn preset_menu(&mut self, ui: &mut egui::Ui, k: &mut Key) {
        menu(ui, tr!("Preset…"), |ui| {
            if let Some(p) = preset_items(ui) {
                *k = p.to_key();
            }
            ui.menu_button(tr!("Applicazioni"), |ui| {
                if let Some(a) = self.app_items(ui) {
                    *k = a.to_key();
                }
            });
        });
    }

    /// Elenco app con ricerca; restituisce quella scelta.
    fn app_items(&mut self, ui: &mut egui::Ui) -> Option<&DesktopApp> {
        ui.horizontal(|ui| {
            let l = ui.label(tr!("Cerca app"));
            ui.add(TextEdit::singleline(&mut self.app_search).hint_text(tr!("nome…")).desired_width(170.0)).labelled_by(l.id);
        });
        let q = self.app_search.to_lowercase();
        let mut chosen = None;
        egui::ScrollArea::vertical().max_height(340.0).show(ui, |ui| {
            for a in self.apps.iter().filter(|a| a.name.to_lowercase().contains(&q)) {
                if ui.button(&a.name).clicked() {
                    chosen = Some(a);
                    ui.close();
                }
            }
        });
        chosen
    }

    fn icon_button(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, icon: &str, tip: &str, enabled: bool) -> bool {
        let btn = match self.icon_image(ctx, icon, 18.0) {
            Some(img) => egui::Button::image(img.alt_text(tip)),
            None => egui::Button::new(tip),
        };
        ui.add_enabled(enabled, btn).on_hover_text(tip).clicked()
    }

    fn steps_editor(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, k: &mut Key) {
        ui.label(
            RichText::new(tr!("I passi partono in ordine. Con \"Apri sul workspace\" vai su quel workspace e l'app si apre lì, anche se ci mette un po'."))
                .color(c32(self.theme.on_surface_variant)),
        );
        let n = k.steps.len();
        let mut op: Option<(usize, i32)> = None;
        for (idx, step) in k.steps.iter_mut().enumerate() {
            egui::Frame::group(ui.style())
                .fill(c32(self.theme.surface_container))
                .stroke(Stroke::new(1.0, c32(self.theme.outline_variant)))
                .corner_radius(CornerRadius::same(12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let num = ui.label(RichText::new(format!("{}.", idx + 1)).strong().color(c32(self.theme.primary)));
                        let mut kind = StepKind::of(step);
                        egui::ComboBox::from_id_salt(("step-kind", idx))
                            .width(170.0)
                            .selected_text(kind.label())
                            .show_ui(ui, |ui| {
                                for sk in StepKind::ALL {
                                    ui.selectable_value(&mut kind, sk, sk.label());
                                }
                            })
                            .response
                            .labelled_by(num.id);
                        if kind != StepKind::of(step) {
                            *step = kind.new_step();
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if self.icon_button(ui, ctx, "delete", tr!("Rimuovi passo"), n > 1) {
                                op = Some((idx, 0));
                            }
                            if self.icon_button(ui, ctx, "arrow_downward", tr!("Sposta giù"), idx + 1 < n) {
                                op = Some((idx, 1));
                            }
                            if self.icon_button(ui, ctx, "arrow_upward", tr!("Sposta su"), idx > 0) {
                                op = Some((idx, -1));
                            }
                        });
                    });
                    match step {
                        Step::Exec { exec, workspace, background, wait } => {
                            ui.add(TextEdit::singleline(exec).code_editor().hint_text(tr!("comando, es. spotify")).desired_width(f32::INFINITY));
                            ui.horizontal(|ui| {
                                let l = ui.label(tr!("Apri sul workspace"));
                                let mut w = workspace.clone().unwrap_or_default();
                                if ui.add(TextEdit::singleline(&mut w).hint_text("attuale").desired_width(70.0)).labelled_by(l.id).changed() {
                                    *workspace = (!w.trim().is_empty()).then_some(w);
                                }
                                ui.checkbox(wait, tr!("Aspetta che finisca"));
                            });
                            if workspace.is_some() {
                                ui.checkbox(background, tr!("In background (apri lì ma non spostarti)"));
                            }
                            ui.horizontal(|ui| {
                                menu(ui, tr!("App…"), |ui| {
                                    if let Some(a) = self.app_items(ui) {
                                        *exec = a.exec.clone();
                                    }
                                });
                                menu(ui, tr!("Preset…"), |ui| {
                                    if let Some(p) = preset_items(ui) {
                                        *exec = p.exec.to_string();
                                    }
                                });
                            });
                        }
                        Step::CloseWindows { scope, workspace, special } => {
                            let scopes: [(CloseScope, &str); 3] = [
                                (CloseScope::Current, tr!("Del workspace attuale")),
                                (CloseScope::Workspace, tr!("Di un workspace")),
                                (CloseScope::All, tr!("Tutte")),
                            ];
                            let current = scopes.iter().find(|(s, _)| s == scope).map_or("", |(_, l)| l);
                            ui.horizontal(|ui| {
                                let l = ui.label(tr!("Finestre"));
                                egui::ComboBox::from_id_salt(("close-scope", idx))
                                    .width(190.0)
                                    .selected_text(current)
                                    .show_ui(ui, |ui| {
                                        for (s, lbl) in scopes {
                                            ui.selectable_value(scope, s, lbl);
                                        }
                                    })
                                    .response
                                    .labelled_by(l.id);
                                if *scope == CloseScope::Workspace {
                                    let mut w = workspace.clone().unwrap_or_default();
                                    let r = ui.add(TextEdit::singleline(&mut w).hint_text(tr!("es. 2")).desired_width(90.0));
                                    if r.labelled_by(l.id).changed() {
                                        *workspace = (!w.trim().is_empty()).then_some(w);
                                    }
                                }
                            });
                            if *scope != CloseScope::Current {
                                ui.checkbox(special, tr!("Anche i workspace speciali (musica, social, ...)"));
                            }
                            ui.label(
                                RichText::new(tr!("Le app ricevono una normale richiesta di chiusura (possono chiederti di salvare). L'editor cyd-deck resta aperto."))
                                    .size(12.0)
                                    .color(c32(self.theme.on_surface_variant)),
                            );
                        }
                        Step::Workspace { workspace } => {
                            ui.horizontal(|ui| {
                                let l = ui.label(tr!("Workspace"));
                                ui.add(TextEdit::singleline(workspace).hint_text(tr!("1, +1, special:music")).desired_width(170.0))
                                    .labelled_by(l.id);
                            });
                        }
                        Step::Delay { ms } => {
                            ui.horizontal(|ui| {
                                let l = ui.label(tr!("Attendi"));
                                ui.add(egui::DragValue::new(ms).range(0..=60_000).speed(10).suffix(" ms")).labelled_by(l.id);
                            });
                        }
                        Step::Action { action } => {
                            let mut options: Vec<(String, String)> = vec![
                                ("next-page".into(), tr!("Pagina successiva").into()),
                                ("prev-page".into(), tr!("Pagina precedente").into()),
                                ("backlight-off".into(), tr!("Spegni lo schermo").into()),
                            ];
                            options.extend(self.cfg.pages.iter().map(|p| (format!("page:{}", p.name), tr!("Vai a: {}", p.name))));
                            let current = options.iter().find(|(v, _)| v == action).map(|(_, l)| l.clone()).unwrap_or_else(|| action.clone());
                            ui.horizontal(|ui| {
                                let l = ui.label(tr!("Azione"));
                                egui::ComboBox::from_id_salt(("step-action", idx))
                                    .width(200.0)
                                    .selected_text(current)
                                    .show_ui(ui, |ui| {
                                        for (v, lbl) in &options {
                                            ui.selectable_value(action, v.clone(), lbl);
                                        }
                                    })
                                    .response
                                    .labelled_by(l.id);
                            });
                        }
                    }
                });
        }
        match op {
            Some((i, 0)) => drop(k.steps.remove(i)),
            Some((i, d)) => k.steps.swap(i, (i as i32 + d) as usize),
            None => {}
        }

        ui.horizontal(|ui| {
            let l = ui.label(tr!("Pausa tra un passo e l'altro"));
            ui.add(egui::DragValue::new(&mut k.step_delay).range(0..=10_000).speed(10).suffix(" ms")).labelled_by(l.id);
            ui.label(RichText::new(tr!("0 = nessuna")).color(c32(self.theme.on_surface_variant)));
        });
        ui.horizontal(|ui| {
            menu(ui, tr!("+ Aggiungi passo"), |ui| {
                ui.menu_button(tr!("Apri un'app"), |ui| {
                    if let Some(a) = self.app_items(ui) {
                        k.steps.push(Step::Exec { exec: a.exec.clone(), workspace: None, background: false, wait: false });
                    }
                });
                ui.menu_button(tr!("Comando da preset"), |ui| {
                    if let Some(p) = preset_items(ui) {
                        k.steps.push(Step::Exec { exec: p.exec.into(), workspace: None, background: false, wait: false });
                    }
                });
                for sk in StepKind::ALL {
                    let text = if sk == StepKind::Exec { tr!("Comando personalizzato") } else { sk.label() };
                    if ui.button(text).clicked() {
                        // "Chiudi finestre" serve prima di aprire le app: va in cima.
                        if sk == StepKind::CloseWindows {
                            k.steps.insert(0, sk.new_step());
                        } else {
                            k.steps.push(sk.new_step());
                        }
                        ui.close();
                    }
                }
            });
            if ui.button(tr!("Prova macro")).on_hover_text(tr!("Esegue i passi adesso (le azioni del deck vengono saltate)")).clicked() {
                macros::run(k.steps.clone(), k.step_delay, |_| {});
            }
        });
    }

    fn test_state(&self, k: &Key, ctx: &egui::Context) {
        let (state, label_cmd) = (k.state.clone(), k.label_cmd.clone());
        let out = self.test_result.clone();
        *out.lock().unwrap() = Some(tr!("in esecuzione…").into());
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let mut lines = Vec::new();
            if let Some(c) = state {
                let ok = app::shell_output(&c).is_some_and(|o| o.status.success());
                lines.push(tr!("stato: {}", if ok { tr!("ATTIVO") } else { tr!("non attivo") }));
            }
            if let Some(c) = label_cmd {
                let o = app::shell_output(&c).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
                lines.push(tr!("etichetta: \"{}\"", o));
            }
            *out.lock().unwrap() = Some(lines.join("\n"));
            ctx.request_repaint();
        });
    }

    fn icon_picker(&mut self, ctx: &egui::Context) {
        let Some(target) = self.picker else { return };
        let mut open = true;
        let mut chosen = None;
        egui::Window::new(tr!("Scegli un'icona"))
            .open(&mut open)
            .collapsible(false)
            .default_size([620.0, 520.0])
            .show(ctx, |ui| {
                let r = ui.add(
                    TextEdit::singleline(&mut self.icon_search)
                        .hint_text(tr!("Cerca in inglese: volume, music, home, light…"))
                        .desired_width(f32::INFINITY),
                );
                r.request_focus();
                let q = self.icon_search.trim().to_lowercase().replace(' ', "_");
                let matches: Vec<String> = self.icon_names.iter().filter(|n| n.contains(&q)).cloned().collect();
                let shown = matches.len().min(ICON_RESULTS);
                ui.label(
                    RichText::new(if matches.len() > shown {
                        tr!("{} icone (mostrate le prime {}, affina la ricerca)", matches.len(), shown)
                    } else {
                        tr!("{} icone", matches.len())
                    })
                    .color(c32(self.theme.on_surface_variant)),
                );
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        for name in &matches[..shown] {
                            let Some(img) = self.icon_image(ctx, name, 32.0) else { continue };
                            if ui.add(egui::Button::image(img).min_size(vec2(54.0, 54.0))).on_hover_text(name).clicked() {
                                chosen = Some(name.clone());
                            }
                        }
                    });
                });
            });
        if let (Some(name), Some(i)) = (chosen, self.selected) {
            let mut k = self.cfg.pages[self.page].keys.get(i).cloned().unwrap_or_default();
            match target {
                IconTarget::Normal => k.icon = Some(name),
                IconTarget::Active => k.icon_active = Some(name),
            }
            self.set_key(i, k);
            open = false;
        }
        if !open {
            self.picker = None;
        }
    }

    // ---------------------------------------------------------------- store

    /// Preset, plugin e app installate, nell'ordine delle categorie.
    fn store_items(&self) -> Vec<Plugin> {
        let ha = self.ha.lock().unwrap().plugins.clone().unwrap_or_default();
        self.catalog.iter().cloned().chain(ha).chain(self.apps.iter().map(DesktopApp::to_plugin)).collect()
    }

    /// Legge da Home Assistant (in background) le entità, o prova solo il collegamento.
    fn ha_fetch(&self, only_check: bool) {
        let mut st = self.ha.lock().unwrap();
        if st.busy {
            return;
        }
        st.busy = true;
        drop(st);
        let (state, cfg) = (self.ha.clone(), self.cfg.clone());
        std::thread::spawn(move || {
            let res = ha::Client::from_config(&cfg, secrets::Prompt::Always).and_then(|c| {
                let name = c.check()?;
                let plugins = if only_check { None } else { Some(c.states()?.iter().filter_map(ha::plugin).collect::<Vec<_>>()) };
                Ok((name, plugins))
            });
            let mut st = state.lock().unwrap();
            st.busy = false;
            match res {
                Ok((name, plugins)) => {
                    st.check = Some(Ok(name));
                    st.error = None;
                    if let Some(mut p) = plugins {
                        p.sort_by(|a, b| a.category.cmp(&b.category).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
                        st.plugins = Some(p);
                    }
                }
                Err(e) => {
                    st.check = Some(Err(format!("{e:#}")));
                    st.error = Some(format!("{e:#}"));
                }
            }
        });
    }

    /// Crea, sblocca o blocca il portachiavi in background, poi ne rilegge lo stato.
    fn keyring_action(&self, action: fn() -> Result<()>) {
        let mut st = self.keyring.lock().unwrap();
        if st.busy {
            return;
        }
        st.busy = true;
        drop(st);
        let state = self.keyring.clone();
        std::thread::spawn(move || {
            let res = action();
            let status = secrets::status().ok();
            let mut st = state.lock().unwrap();
            st.busy = false;
            st.status = status;
            st.error = res.err().map(|e| format!("{e:#}"));
        });
    }

    fn keyring_settings(&mut self, ui: &mut egui::Ui) {
        section(ui, &self.theme, tr!("Portachiavi"));
        if self.keyring.lock().unwrap().status.is_none() && !self.keyring.lock().unwrap().busy {
            self.keyring_action(|| Ok(()));
        }
        let st = self.keyring.lock().unwrap();
        let (busy, status, error) = (st.busy, st.status, st.error.clone());
        drop(st);
        let text = match status {
            Some(secrets::Status::Missing) => {
                tr!("Token e password del Wi-Fi stanno nel portachiavi di login, aperto quando entri. Con un portachiavi \
                 dedicato, che ha una password sua, restano chiusi finché non lo sblocchi: la finestra compare al primo uso.")
            }
            Some(secrets::Status::Locked) => tr!("Portachiavi cyd-deck chiuso: i tasti di Home Assistant aspettano che lo sblocchi."),
            Some(secrets::Status::Unlocked) => tr!("Portachiavi cyd-deck aperto fino al logout, o finché non lo blocchi."),
            None => "",
        };
        ui.label(RichText::new(text).color(c32(self.theme.on_surface_variant)));
        ui.horizontal(|ui| {
            match status {
                Some(secrets::Status::Missing) => {
                    if ui.add_enabled(!busy, egui::Button::new(tr!("Crea portachiavi protetto"))).on_hover_text(tr!("Ti chiede la nuova password e ci sposta i segreti")).clicked() {
                        self.keyring_action(secrets::create);
                    }
                }
                Some(secrets::Status::Locked) => {
                    if ui.add_enabled(!busy, egui::Button::new(tr!("Sblocca"))).clicked() {
                        self.keyring_action(secrets::unlock);
                    }
                }
                Some(secrets::Status::Unlocked) => {
                    if ui.add_enabled(!busy, egui::Button::new(tr!("Blocca ora"))).clicked() {
                        self.keyring_action(secrets::lock);
                    }
                }
                None => {}
            }
            if busy {
                ui.spinner();
            }
            if let Some(e) = &error {
                ui.label(RichText::new(e).color(c32(self.theme.error)));
            }
        });
    }

    fn printer_settings(&mut self, ui: &mut egui::Ui) {
        section(ui, &self.theme, tr!("Stampante 3D (Bambu Lab)"));
        ui.label(
            RichText::new(tr!("Collegamento in LAN. Il codice di accesso è sullo schermo della stampante (Impostazioni, WLAN) e va nel portachiavi."))
                .color(c32(self.theme.on_surface_variant)),
        );
        ui.add_space(4.0);
        let saved = *self.bambu_code_saved.get_or_insert_with(|| secrets::has(secrets::BAMBU_CODE));
        egui::Grid::new("printer").num_columns(2).spacing([24.0, 10.0]).show(ui, |ui| {
            let label = ui.label(tr!("Indirizzo IP"));
            ui.add(TextEdit::singleline(&mut self.cfg.printer.ip).desired_width(260.0).hint_text("192.168.0.x")).labelled_by(label.id);
            ui.end_row();
            let label = ui.label(tr!("Numero di serie"));
            ui.add(TextEdit::singleline(&mut self.cfg.printer.serial).desired_width(260.0)).labelled_by(label.id);
            ui.end_row();
            let label = ui.label(tr!("Codice di accesso"));
            ui.horizontal(|ui| {
                let hint = if saved { tr!("salvato ✓ (scrivine uno nuovo per sostituirlo)") } else { tr!("8 caratteri") };
                ui.add(TextEdit::singleline(&mut self.bambu_code_input).password(true).desired_width(260.0).hint_text(hint))
                    .labelled_by(label.id);
                let code = self.bambu_code_input.trim().to_string();
                if ui.add_enabled(!code.is_empty(), egui::Button::new(tr!("Salva"))).clicked() {
                    match secrets::set(secrets::BAMBU_CODE, &code) {
                        Ok(()) => {
                            self.bambu_code_saved = Some(true);
                            self.set_status(tr!("Codice della stampante salvato nel portachiavi"), false);
                        }
                        Err(e) => self.set_status(tr!("Codice non salvato: {}", format!("{e:#}")), true),
                    }
                    self.bambu_code_input.clear();
                }
                if saved && ui.button(tr!("Elimina")).clicked() {
                    match secrets::set(secrets::BAMBU_CODE, "") {
                        Ok(()) => self.bambu_code_saved = Some(false),
                        Err(e) => self.set_status(format!("{e:#}"), true),
                    }
                }
            });
            ui.end_row();
        });
        ui.horizontal(|ui| {
            let (busy, result) = self.printer_check.lock().unwrap().clone();
            let ready = saved && printer::configured(&self.cfg) && self.cfg == self.saved;
            let btn = ui.add_enabled(!busy && ready, egui::Button::new(tr!("Prova collegamento")));
            let btn = if self.cfg != self.saved { btn.on_disabled_hover_text(tr!("Prima salva la config")) } else { btn };
            if btn.clicked() {
                let state = self.printer_check.clone();
                state.lock().unwrap().0 = true;
                std::thread::spawn(move || {
                    let res = printer::check().map_err(|e| format!("{e:#}"));
                    *state.lock().unwrap() = (false, Some(res));
                });
            }
            if busy {
                ui.spinner();
            }
            match result {
                Some(Ok(s)) => drop(ui.label(RichText::new(s).color(c32(self.theme.primary)))),
                Some(Err(e)) => drop(ui.label(RichText::new(e).color(c32(self.theme.error)))),
                None => {}
            }
        });
    }

    fn ha_settings(&mut self, ui: &mut egui::Ui) {
        section(ui, &self.theme, tr!("Home Assistant"));
        ui.label(
            RichText::new(tr!("Le entità compaiono nello Store. Il token sta nel portachiavi del sistema, non nella config: qui non si rivede."))
                .color(c32(self.theme.on_surface_variant)),
        );
        ui.add_space(4.0);
        let saved = *self.ha_token_saved.get_or_insert_with(|| secrets::has(secrets::HA_TOKEN));
        egui::Grid::new("ha").num_columns(2).spacing([24.0, 10.0]).show(ui, |ui| {
            let label = ui.label(tr!("Indirizzo"));
            ui.add(TextEdit::singleline(&mut self.cfg.homeassistant.url).desired_width(260.0).hint_text("http://homeassistant.local:8123"))
                .labelled_by(label.id);
            ui.end_row();
            let label = ui.label(tr!("Token"));
            ui.horizontal(|ui| {
                let hint = if saved { tr!("salvato ✓ (scrivine uno nuovo per sostituirlo)") } else { tr!("incolla il token a lunga durata") };
                ui.add(TextEdit::singleline(&mut self.ha_token_input).password(true).desired_width(260.0).hint_text(hint))
                    .labelled_by(label.id);
                let token = self.ha_token_input.trim().to_string();
                if ui.add_enabled(!token.is_empty(), egui::Button::new(tr!("Salva"))).clicked() {
                    match secrets::set(secrets::HA_TOKEN, &token) {
                        Ok(()) => {
                            self.ha_token_saved = Some(true);
                            self.set_status(tr!("Token salvato nel portachiavi"), false);
                            ha::reset();
                            self.ha_fetch(true);
                        }
                        Err(e) => self.set_status(tr!("Token non salvato: {}", format!("{e:#}")), true),
                    }
                    self.ha_token_input.clear();
                }
                if saved && ui.button(tr!("Elimina")).clicked() {
                    match secrets::set(secrets::HA_TOKEN, "") {
                        Ok(()) => {
                            self.ha_token_saved = Some(false);
                            self.ha.lock().unwrap().plugins = None;
                            self.set_status(tr!("Token eliminato dal portachiavi"), false);
                        }
                        Err(e) => self.set_status(format!("{e:#}"), true),
                    }
                }
            });
            ui.end_row();
        });
        ui.horizontal(|ui| {
            let busy = self.ha.lock().unwrap().busy;
            if ui.add_enabled(!busy && saved && !self.cfg.homeassistant.url.is_empty(), egui::Button::new(tr!("Prova collegamento"))).clicked() {
                self.ha_fetch(true);
            }
            if busy {
                ui.spinner();
            }
            match &self.ha.lock().unwrap().check {
                Some(Ok(name)) => ui.label(RichText::new(tr!("Collegato a {}", name)).color(c32(self.theme.primary))),
                Some(Err(e)) => ui.label(RichText::new(e).color(c32(self.theme.error))),
                None => ui.label(""),
            };
        });
    }

    fn reload_plugins(&mut self) {
        (self.catalog, self.catalog_errors) = presets::catalog(&presets::plugins_dir(&self.cfg_path));
        let n = self.catalog.iter().filter(|p| p.user).count();
        self.set_status(tr!("Plugin ricaricati: {} dai file", n), false);
    }

    /// Apre la cartella dei plugin, creandola con un esempio se non c'è.
    fn open_plugins_dir(&mut self) {
        let dir = presets::plugins_dir(&self.cfg_path);
        if !dir.exists() {
            let made = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(dir.join("esempio.toml"), presets::EXAMPLE_PLUGIN));
            if let Err(e) = made {
                self.set_status(tr!("Non riesco a creare {}: {}", dir.display(), e), true);
                return;
            }
            self.reload_plugins();
        }
        let _ = std::process::Command::new("xdg-open").arg(&dir).spawn();
    }

    fn store_categories(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        let ha_ready = !self.cfg.homeassistant.url.is_empty();
        if ha_ready && self.ha.lock().unwrap().plugins.is_none() && self.ha.lock().unwrap().error.is_none() {
            self.ha_fetch(false);
        }
        let items = self.store_items();
        let mut cats: Vec<&str> = Vec::new();
        for p in &items {
            if !cats.contains(&p.category.as_str()) {
                cats.push(&p.category);
            }
        }
        section(ui, &self.theme, tr!("Categorie"));
        let mut cat = self.store_cat.clone();
        ui.selectable_value(&mut cat, None, tr!("Tutte  ({})", items.len()));
        for c in cats {
            let n = items.iter().filter(|p| p.category == c).count();
            ui.selectable_value(&mut cat, Some(c.to_string()), format!("{c}  ({n})"));
        }
        self.store_cat = cat;

        ui.add_space(16.0);
        section(ui, &self.theme, tr!("Plugin tuoi"));
        ui.label(
            RichText::new(tr!("File .toml nella cartella dei plugin: ognuno può contenere più funzioni."))
                .color(c32(self.theme.on_surface_variant)),
        );
        if ui.button(tr!("Apri cartella plugin")).clicked() {
            self.open_plugins_dir();
        }
        if ui.button(tr!("Ricarica plugin")).clicked() {
            self.reload_plugins();
        }
        if ha_ready {
            ui.add_space(16.0);
            section(ui, &self.theme, tr!("Home Assistant"));
            let st = self.ha.lock().unwrap();
            let (busy, error, n) = (st.busy, st.error.clone(), st.plugins.as_ref().map(Vec::len));
            drop(st);
            match (&error, n) {
                (Some(e), _) => drop(ui.label(RichText::new(e).color(c32(self.theme.error)))),
                (None, Some(n)) => drop(ui.label(RichText::new(tr!("{} entità utilizzabili", n)).color(c32(self.theme.on_surface_variant)))),
                _ => {}
            }
            ui.horizontal(|ui| {
                if ui.add_enabled(!busy, egui::Button::new(tr!("Aggiorna da Home Assistant"))).clicked() {
                    self.ha_fetch(false);
                }
                if busy {
                    ui.spinner();
                }
            });
        }
        for e in &self.catalog_errors {
            ui.label(RichText::new(e).color(c32(self.theme.error)));
        }
    }

    fn store(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let key_pages: Vec<usize> = (0..self.cfg.pages.len()).filter(|&i| self.cfg.pages[i].kind == PageKind::Keys).collect();
        if !key_pages.contains(&self.store_page) {
            self.store_page = if key_pages.contains(&self.page) { self.page } else { key_pages.first().copied().unwrap_or(0) };
        }
        let page_name = |cfg: &Config, i: usize| if cfg.pages[i].name.is_empty() { tr!("Pagina {}", i + 1) } else { cfg.pages[i].name.clone() };

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let l = ui.label(tr!("Cerca"));
            ui.add(TextEdit::singleline(&mut self.store_search).hint_text(tr!("volume, screenshot, workspace, firefox…")).desired_width(360.0))
                .labelled_by(l.id);
            ui.add_space(16.0);
            ui.label(tr!("Aggiungi a"));
            egui::ComboBox::from_id_salt("store-page").width(180.0).selected_text(page_name(&self.cfg, self.store_page)).show_ui(ui, |ui| {
                for &i in &key_pages {
                    ui.selectable_value(&mut self.store_page, i, page_name(&self.cfg, i));
                }
            });
        });
        ui.add_space(8.0);

        let q = self.store_search.trim().to_lowercase();
        let items: Vec<Plugin> = self
            .store_items()
            .into_iter()
            .filter(|p| self.store_cat.as_ref().is_none_or(|c| &p.category == c) && p.matches(&q))
            .collect();
        if items.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| ui.label(RichText::new(tr!("Nessuna funzione trovata")).color(c32(self.theme.on_surface_variant))));
            return;
        }
        // Comandi già presenti su una pagina del deck.
        let used: HashSet<String> = self.cfg.pages.iter().flat_map(|p| &p.keys).filter_map(|k| k.exec.clone()).collect();

        let mut add = None;
        const CARD: egui::Vec2 = vec2(280.0, 112.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(10.0, 10.0);
            let per_row = ((ui.available_width() + 10.0) / (CARD.x + 10.0)).floor().max(1.0) as usize;
            for (row, chunk) in items.chunks(per_row).enumerate() {
            ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
                for (j, p) in chunk.iter().enumerate() {
                    let n = row * per_row + j;
                    // {battery} è un segnaposto: nell'anteprima si mostra una batteria.
                    let icon = if p.icon.contains("{battery}") { app::battery_icon(Some(80), false).to_string() } else { p.icon.clone() };
                    let img = self.icon_image(&ctx, &icon, 30.0);
                    let on_deck = !p.exec.is_empty() && used.contains(&p.exec);
                    ui.allocate_ui_with_layout(CARD, Layout::top_down(Align::Min), |ui| {
                        egui::Frame::new()
                            .fill(c32(self.theme.surface_container))
                            .corner_radius(CornerRadius::same(12))
                            .inner_margin(10.0)
                            .show(ui, |ui| {
                                ui.set_width(CARD.x - 20.0);
                                ui.set_height(CARD.y - 20.0);
                                ui.horizontal(|ui| {
                                    if let Some(img) = img {
                                        ui.add(img);
                                    }
                                    ui.vertical(|ui| {
                                        ui.label(RichText::new(&p.name).strong().size(15.0));
                                        let tag = if p.user { format!("{} · plugin", p.category) } else { p.category.clone() };
                                        ui.label(RichText::new(tag).size(12.0).color(c32(self.theme.on_surface_variant)));
                                    });
                                });
                                let detail = [&p.description, &p.exec, p.label_cmd.as_ref().unwrap_or(&String::new())]
                                    .into_iter()
                                    .find(|d| !d.is_empty())
                                    .cloned()
                                    .unwrap_or_default();
                                let r = ui.add(egui::Label::new(RichText::new(detail).size(12.0).color(c32(self.theme.on_surface_variant))).truncate());
                                if !p.exec.is_empty() {
                                    r.on_hover_text(&p.exec);
                                }
                                ui.with_layout(Layout::right_to_left(Align::Max), |ui| {
                                    let text = if on_deck { tr!("Aggiungi ancora") } else { tr!("Aggiungi") };
                                    if ui.add(egui::Button::new(text)).on_hover_text(tr!("Aggiungi «{}» al primo posto libero", p.name)).clicked() {
                                        add = Some(n);
                                    }
                                    if on_deck {
                                        ui.label(RichText::new(tr!("già sul deck")).size(12.0).color(c32(self.theme.primary)));
                                    }
                                });
                            });
                    });
                }
            });
            }
        });
        if let Some(n) = add {
            self.store_add(&items[n]);
        }
    }

    /// Mette la funzione nel primo posto libero della pagina scelta nello store.
    fn store_add(&mut self, p: &Plugin) {
        let page = self.store_page.min(self.cfg.pages.len() - 1);
        let (cols, rows) = (self.cfg.layout.cols.max(1), self.cfg.layout.rows.max(1));
        let keys = &self.cfg.pages[page].keys;
        let owners = config::owners(keys, cols, rows);
        let free = (0..owners.len()).find(|&i| owners[i] == i && keys.get(i).is_none_or(|k| *k == Key::default()));
        let name = if self.cfg.pages[page].name.is_empty() { tr!("Pagina {}", page + 1) } else { self.cfg.pages[page].name.clone() };
        let Some(i) = free else {
            self.set_status(tr!("{} è piena: libera un tasto o scegli un'altra pagina", name), true);
            return;
        };
        self.page = page;
        self.selected = Some(i);
        self.set_key(i, p.to_key());
        self.set_status(tr!("«{}» aggiunto a {}, tasto {}", p.name, name, i + 1), false);
    }

    // ---------------------------------------------------------------- impostazioni

    /// Applica subito lingua e colori cambiati (nelle impostazioni o nella guida).
    fn after_settings_change(&mut self, ctx: &egui::Context, before: &Config) {
        if self.cfg.language != before.language {
            crate::i18n::set(&self.cfg.language);
            self.textures.clear();
        }
        if self.cfg.theme != before.theme {
            self.refresh_theme(ctx);
        }
    }

    // ---------------------------------------------------------------- guida rapida

    const INTRO_STEPS: usize = 5;

    fn intro_window(&mut self, ctx: &egui::Context) {
        let Some(step) = self.intro else { return };
        let before = self.cfg.clone();
        let mut next = Some(step);
        let mut finish = false;
        egui::Modal::new(egui::Id::unique("intro")).show(ctx, |ui| {
            ui.set_width(540.0);
            let muted = c32(self.theme.on_surface_variant);
            ui.label(RichText::new(tr!("Passo {} di {}", step + 1, Self::INTRO_STEPS)).color(muted).size(13.0));
            ui.add_space(4.0);
            let title = |ui: &mut egui::Ui, text: &str| {
                ui.label(RichText::new(text).size(22.0).strong().color(c32(self.theme.primary)));
                ui.add_space(8.0);
            };
            match step {
                0 => {
                    title(ui, tr!("Benvenuto in CYD Deck"));
                    ui.label(tr!(
                        "CYD Deck trasforma uno schermo touch ESP32 da pochi euro in un macropad: tasti con icone, pagine, mixer audio e molto altro. Ti servono pochi minuti per partire."
                    ));
                    ui.add_space(14.0);
                    ui.label(tr!("In che lingua vuoi l'interfaccia?"));
                    language_picker(ui, &mut self.cfg.language);
                }
                1 => {
                    title(ui, tr!("Collega la scheda"));
                    ui.label(tr!(
                        "Collega la scheda al PC con un cavo USB-C dati. Se non hai ancora flashato il firmware, segui il README (sezione 1)."
                    ));
                    ui.add_space(10.0);
                    let (port, daemon) = self.intro_check.get_or_insert_with(|| (device::find_port("auto"), daemon_running())).clone();
                    let ok = c32(self.theme.primary);
                    match port {
                        Some(p) => ui.label(RichText::new(format!("{}  ({p})", tr!("Scheda trovata via USB"))).color(ok)),
                        None => ui.label(RichText::new(tr!("Nessuna scheda via USB per ora (va bene anche più tardi, o via Wi-Fi).")).color(muted)),
                    };
                    if daemon {
                        ui.label(RichText::new(tr!("Il programma in background è attivo")).color(ok));
                    } else {
                        ui.label(RichText::new(tr!("Il programma in background non è attivo: avvialo con `cyd-deck` (o all'avvio del desktop).")).color(muted));
                    }
                    ui.add_space(6.0);
                    if ui.button(tr!("Controlla di nuovo")).clicked() {
                        self.intro_check = None;
                    }
                    ui.add_space(6.0);
                    ui.label(RichText::new(tr!("Se il tocco finisce nel tasto sbagliato, chiudi il programma in background e lancia `cyd-deck calibrate`.")).color(muted));
                }
                2 => {
                    title(ui, tr!("Scegli i colori"));
                    theme_picker(ui, &mut self.cfg.theme, &self.scheme_path, &self.theme);
                }
                3 => {
                    title(ui, tr!("Le tue pagine"));
                    ui.label(tr!("Da dove vuoi partire?"));
                    ui.add_space(6.0);
                    for (i, (name, desc)) in [
                        (tr!("Tieni le pagine attuali"), ""),
                        (tr!("Pagine di esempio"), tr!("Media, app e sistema: da modificare come vuoi")),
                        (tr!("Pagina vuota"), tr!("Una griglia vuota da riempire dallo Store")),
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        ui.radio_value(&mut self.intro_pages, i, name);
                        if !desc.is_empty() {
                            ui.indent(("intro-page", i), |ui| ui.label(RichText::new(desc).color(muted).size(13.0)));
                        }
                    }
                }
                _ => {
                    title(ui, tr!("Tutto pronto!"));
                    for line in [
                        tr!("Clic su un tasto per modificarlo, trascinalo per spostarlo."),
                        tr!("Nello Store trovi centinaia di funzioni pronte da aggiungere."),
                        tr!("Con \"+ Pagina\" aggiungi pagine di tasti, audio o widget."),
                        tr!("Il Wi-Fi della scheda si imposta in Impostazioni."),
                        tr!("Le modifiche arrivano sul deck appena salvi."),
                    ] {
                        ui.label(format!("·  {line}"));
                    }
                }
            }
            ui.add_space(18.0);
            ui.horizontal(|ui| {
                if step + 1 < Self::INTRO_STEPS && ui.button(tr!("Salta")).clicked() {
                    finish = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let last = step + 1 == Self::INTRO_STEPS;
                    let label = if last { tr!("Inizia") } else { tr!("Avanti") };
                    let primary = egui::Button::new(RichText::new(label).color(c32(self.theme.on_primary))).fill(c32(self.theme.primary));
                    if ui.add(primary).clicked() {
                        if last {
                            finish = true;
                        } else {
                            next = Some(step + 1);
                        }
                    }
                    if step > 0 && ui.button(tr!("Indietro")).clicked() {
                        next = Some(step - 1);
                    }
                });
            });
        });
        // Le pagine cambiano solo lasciando quel passo, così si può tornare indietro e cambiare idea.
        if step == 3 && next != Some(3) {
            match self.intro_pages {
                1 => self.cfg.pages = config::sample_pages(self.scheme_path.exists()),
                2 => self.cfg.pages = vec![Page { name: tr!("Principale").into(), ..Page::default() }],
                _ => {}
            }
            if self.intro_pages != 0 {
                pad_pages(&mut self.cfg);
                self.page = 0;
                self.selected = None;
                self.textures.clear();
                // Rifarlo tornando indietro sostituirebbe di nuovo le pagine: d'ora in poi le tiene.
                self.intro_pages = 0;
            }
        }
        if finish {
            self.cfg.intro_done = true;
            next = None;
        }
        self.intro = next;
        self.after_settings_change(ctx, &before);
        if self.cfg != before {
            self.edited();
        }
    }

    fn settings(&mut self, ui: &mut egui::Ui) {
        let before = self.cfg.clone();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width(620.0);
            let c = &mut self.cfg;

            section(ui, &self.theme, tr!("Lingua"));
            language_picker(ui, &mut c.language);
            ui.add_space(14.0);

            section(ui, &self.theme, tr!("Tema"));
            theme_picker(ui, &mut c.theme, &self.scheme_path, &self.theme);
            ui.add_space(6.0);
            if ui.button(tr!("Riapri la guida rapida")).clicked() {
                self.intro = Some(0);
                self.intro_check = None;
            }
            let c = &mut self.cfg;
            ui.add_space(14.0);

            section(ui, &self.theme, tr!("Griglia"));
            egui::Grid::new("layout").num_columns(2).spacing([24.0, 10.0]).show(ui, |ui| {
                ui.label(tr!("Colonne"));
                ui.add(egui::DragValue::new(&mut c.layout.cols).range(1..=6));
                ui.end_row();
                ui.label(tr!("Righe"));
                ui.add(egui::DragValue::new(&mut c.layout.rows).range(1..=5));
                ui.end_row();
                ui.label(tr!("Spazio tra i tasti"));
                ui.add(egui::Slider::new(&mut c.layout.gap, 0..=20).suffix(" px"));
                ui.end_row();
                ui.label(tr!("Arrotondamento"));
                ui.add(egui::Slider::new(&mut c.layout.radius, 0.0..=40.0).suffix(" px"));
                ui.end_row();
            });
            ui.add_space(14.0);

            section(ui, &self.theme, tr!("Schermo"));
            egui::Grid::new("screen").num_columns(2).spacing([24.0, 10.0]).show(ui, |ui| {
                let label = ui.label(tr!("Orientamento"));
                let rot: [&str; 4] = [tr!("Verticale"), tr!("Orizzontale"), tr!("Verticale capovolto"), tr!("Orizzontale capovolto")];
                egui::ComboBox::from_id_salt("rot").selected_text(rot[c.device.rotation as usize % 4]).show_ui(ui, |ui| {
                    for (i, n) in rot.iter().enumerate() {
                        ui.selectable_value(&mut c.device.rotation, i as u8, *n);
                    }
                })
                .response
                .labelled_by(label.id);
                ui.end_row();
                ui.label(tr!("Spegnimento automatico"));
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut c.device.idle_timeout).range(0..=86400).suffix(" s"));
                    ui.label(RichText::new(tr!("0 = mai")).color(c32(self.theme.on_surface_variant)));
                });
                ui.end_row();
                ui.label(tr!("Colori"));
                ui.horizontal(|ui| {
                    ui.checkbox(&mut c.device.invert, tr!("Inversione"));
                    ui.checkbox(&mut c.device.bgr, tr!("Ordine BGR"));
                });
                ui.end_row();
                ui.label(tr!("Porta seriale"));
                ui.add(TextEdit::singleline(&mut c.device.port).desired_width(200.0).hint_text("auto"));
                ui.end_row();
            });
            ui.add_space(14.0);

            section(ui, &self.theme, tr!("Barra in alto"));
            ui.checkbox(&mut c.bar.show, tr!("Mostra la barra di stato sul deck"));
            ui.add_enabled_ui(c.bar.show, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.checkbox(&mut c.bar.page, tr!("Nome pagina"));
                    ui.checkbox(&mut c.bar.clock, tr!("Ora"));
                    ui.checkbox(&mut c.bar.link, tr!("USB / Wi-Fi"));
                    ui.checkbox(&mut c.bar.battery, tr!("Batteria"));
                });
            });
            ui.add_space(14.0);

            section(ui, &self.theme, tr!("Wi-Fi della scheda"));
            ui.label(
                RichText::new(tr!(
                    "Collega la scheda via USB una volta con il programma acceso: riceve la rete e da lì funziona anche senza cavo. Solo reti a 2,4 GHz."
                ))
                .color(c32(self.theme.on_surface_variant)),
            );
            ui.add_space(4.0);
            egui::Grid::new("wifi").num_columns(2).spacing([24.0, 10.0]).show(ui, |ui| {
                let label = ui.label(tr!("Rete (SSID)"));
                ui.horizontal(|ui| {
                    ui.add(TextEdit::singleline(&mut c.wifi.ssid).desired_width(220.0).hint_text(tr!("vuoto = Wi-Fi spento")))
                        .labelled_by(label.id);
                    if ui.button(tr!("Rete attuale")).on_hover_text(tr!("Usa la rete a cui è collegato il PC")).clicked() {
                        if let Some(ssid) = current_ssid() {
                            c.wifi.ssid = ssid;
                        }
                    }
                });
                ui.end_row();
                let label = ui.label(tr!("Password"));
                let hint = if c.wifi.locked { tr!("nel portachiavi chiuso") } else { "" };
                ui.add(TextEdit::singleline(&mut c.wifi.password).password(true).desired_width(220.0).hint_text(hint)).labelled_by(label.id);
                ui.end_row();
                let label = ui.label(tr!("Indirizzo fisso"));
                ui.add(TextEdit::singleline(&mut c.wifi.address).desired_width(220.0).hint_text(tr!("vuoto = la trova da solo")))
                    .labelled_by(label.id);
                ui.end_row();
                ui.label(tr!("Codice di abbinamento"));
                ui.horizontal(|ui| {
                    let shown = if c.wifi.token.is_empty() { "-".to_string() } else { format!("{}…", &c.wifi.token[..8.min(c.wifi.token.len())]) };
                    ui.label(RichText::new(shown).monospace());
                    if ui.button(tr!("Nuovo codice")).on_hover_text(tr!("La scheda va ricollegata via USB per riceverlo")).clicked() {
                        c.wifi.token = config::new_token();
                    }
                });
                ui.end_row();
            });
            if !c.wifi.ssid.is_empty() && c.wifi.token.is_empty() {
                c.wifi.token = config::new_token();
            }
            ui.add_space(14.0);

            self.ha_settings(ui);
            ui.add_space(14.0);
            self.printer_settings(ui);
            ui.add_space(14.0);
            self.keyring_settings(ui);
            let c = &mut self.cfg;
            ui.add_space(14.0);

            section(ui, &self.theme, tr!("Touch"));
            ui.horizontal(|ui| {
                ui.checkbox(&mut c.touch.swap_xy, tr!("Scambia X/Y"));
                ui.checkbox(&mut c.touch.invert_x, tr!("Inverti X"));
                ui.checkbox(&mut c.touch.invert_y, tr!("Inverti Y"));
            });
            ui.label(
                RichText::new(tr!("Per calibrarlo in automatico chiudi il demone e lancia `cyd-deck calibrate`."))
                    .color(c32(self.theme.on_surface_variant)),
            );
            ui.add_space(14.0);

            section(ui, &self.theme, tr!("Aggiornamento stati"));
            ui.horizontal(|ui| {
                ui.label(tr!("Ogni"));
                ui.add(egui::DragValue::new(&mut c.layout.poll_interval).range(0.2..=60.0).speed(0.1).suffix(" s"));
                ui.label(RichText::new(tr!("per i tasti con comando di stato o etichetta")).color(c32(self.theme.on_surface_variant)));
            });
            ui.add_space(14.0);

            section(ui, &self.theme, tr!("Font"));
            egui::Grid::new("fonts").num_columns(2).spacing([24.0, 10.0]).show(ui, |ui| {
                ui.label(tr!("Icone"));
                ui.add(TextEdit::singleline(&mut c.layout.icon_font).desired_width(260.0));
                ui.end_row();
                ui.label(tr!("Testo"));
                ui.add(TextEdit::singleline(&mut c.layout.text_font).desired_width(260.0));
                ui.end_row();
            });
            if ui.button(tr!("Applica font all'anteprima")).clicked() {
                match Fonts::load(&self.cfg.layout.icon_font, &self.cfg.layout.text_font) {
                    Ok(f) => {
                        self.icon_names = icons::ligature_names(f.icon_data);
                        self.fonts = f;
                        self.textures.clear();
                    }
                    Err(e) => self.set_status(tr!("Font: {}", format!("{e:#}")), true),
                }
            }
        });
        self.after_settings_change(ui.ctx(), &before);
        if self.cfg != before {
            if self.cfg.layout != before.layout {
                pad_pages(&mut self.cfg);
                self.textures.clear();
            }
            self.edited();
        }
    }
}

/// Menu a tendina con le lingue; applica subito la scelta.
fn language_picker(ui: &mut egui::Ui, lang: &mut String) {
    let current = if lang.is_empty() { "auto" } else { lang.as_str() };
    let name = |code: &str| crate::i18n::LANGUAGES.iter().find(|(c, _)| *c == code).map_or("?", |(_, n)| *n);
    let mut chosen = None;
    egui::ComboBox::from_id_salt("language").width(220.0).selected_text(name(current)).show_ui(ui, |ui| {
        for (code, n) in crate::i18n::LANGUAGES {
            if ui.selectable_label(current == *code, *n).clicked() {
                chosen = Some(code.to_string());
            }
        }
    });
    if let Some(code) = chosen {
        crate::i18n::set(&code);
        *lang = code;
    }
}

fn to_hex([r, g, b]: [f32; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", (r * 255.0).round() as u8, (g * 255.0).round() as u8, (b * 255.0).round() as u8)
}

/// Temi pronti come pallini colorati, un colore a piacere e chiaro/scuro.
fn theme_picker(ui: &mut egui::Ui, th: &mut config::ThemeCfg, scheme: &Path, now: &Theme) {
    let caelestia = scheme.exists();
    ui.label(RichText::new(tr!("Scegli un tema pronto o un colore tuo: tutto il resto si adatta da solo.")).color(c32(now.on_surface_variant)));
    ui.add_space(4.0);
    if caelestia {
        let mut on = th.style != "custom";
        if ui
            .checkbox(&mut on, tr!("Colori di Caelestia (automatici)"))
            .on_hover_text(tr!("Usa lo schema di Caelestia e cambia insieme allo sfondo"))
            .changed()
        {
            th.style = if on { "auto" } else { "custom" }.into();
        }
    } else {
        ui.label(RichText::new(tr!("Caelestia non trovato: si usa il tema scelto qui sotto.")).color(c32(now.on_surface_variant)));
    }
    ui.add_enabled_ui(!caelestia || th.style == "custom", |ui| {
        ui.horizontal_wrapped(|ui| {
            for (name, hex) in theme::PRESETS {
                let Some(rgb) = theme::parse_hex(hex) else { continue };
                let selected = th.accent.eq_ignore_ascii_case(hex);
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(34.0, 34.0), egui::Sense::click());
                let painter = ui.painter();
                if selected {
                    painter.circle_stroke(rect.center(), 16.0, egui::Stroke::new(2.5, c32(now.on_surface)));
                }
                painter.circle_filled(rect.center(), 12.0, c32(rgb));
                if resp.on_hover_text(crate::i18n::t(name)).clicked() {
                    th.accent = hex.to_string();
                }
            }
            ui.add_space(6.0);
            let mut rgb = theme::parse_hex(&th.accent).unwrap_or(now.primary);
            if ui.color_edit_button_rgb(&mut rgb).on_hover_text(tr!("Personalizzato")).changed() {
                th.accent = to_hex(rgb);
            }
        });
        ui.horizontal(|ui| {
            ui.selectable_value(&mut th.light, false, tr!("Scuro"));
            ui.selectable_value(&mut th.light, true, tr!("Chiaro"));
        });
    });
}


/// Il programma in background (`cyd-deck` o `cyd-deck run`) è in esecuzione?
fn daemon_running() -> bool {
    app::shell_output("pgrep -f 'cyd-deck( run)?$'").is_some_and(|o| o.status.success())
}

/// Pulsante menu che si chiude solo cliccando fuori o scegliendo una voce:
/// così si può cliccare nella ricerca delle app senza chiuderlo.
fn menu(ui: &mut egui::Ui, text: &str, add: impl FnOnce(&mut egui::Ui)) {
    let config = MenuConfig::new().close_behavior(PopupCloseBehavior::CloseOnClickOutside);
    MenuButton::new(text).config(config).ui(ui, add);
}

/// Classi delle finestre aperte in Hyprland (senza l'editor stesso).
fn open_window_classes() -> Vec<String> {
    let Some(out) = app::shell_output("hyprctl clients -j") else { return Vec::new() };
    let v: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap_or_default();
    let mut classes: Vec<String> = v
        .iter()
        .filter_map(|c| c["class"].as_str())
        .filter(|c| !c.is_empty() && *c != "cyd-deck")
        .map(str::to_string)
        .collect();
    classes.sort_by_key(|c| c.to_lowercase());
    classes.dedup();
    classes
}

/// SSID della rete Wi-Fi attiva del PC, da NetworkManager.
fn current_ssid() -> Option<String> {
    let out = app::shell_output("nmcli -t -f active,ssid dev wifi")?;
    String::from_utf8_lossy(&out.stdout).lines().find_map(|l| l.strip_prefix("yes:").map(|s| s.replace("\\:", ":")))
}

/// Sottomenu con i gruppi di preset; restituisce quello scelto.
fn preset_items(ui: &mut egui::Ui) -> Option<&'static Preset> {
    let mut chosen = None;
    for g in presets::GROUPS {
        ui.menu_button(crate::i18n::t(g.name), |ui| {
            for p in g.presets {
                if ui.button(crate::i18n::t(p.name)).clicked() {
                    chosen = Some(p);
                    ui.close();
                }
            }
        });
    }
    chosen
}

fn section(ui: &mut egui::Ui, t: &Theme, title: &str) {
    ui.label(RichText::new(title).size(15.0).strong().color(c32(t.primary)));
    ui.add_space(2.0);
}

/// Allunga ogni pagina fino al numero di posti della griglia.
fn pad_pages(cfg: &mut Config) {
    if cfg.pages.is_empty() {
        cfg.pages.push(Page { name: tr!("Principale").into(), ..Page::default() });
    }
    let slots = (cfg.layout.cols.max(1) * cfg.layout.rows.max(1)) as usize;
    for p in cfg.pages.iter_mut().filter(|p| p.kind == PageKind::Keys) {
        if p.keys.len() < slots {
            p.keys.resize(slots, Key::default());
        }
    }
}

/// Colori dell'interfaccia dallo schema Material You di Caelestia.
fn apply_theme(ctx: &egui::Context, t: &Theme) {
    let mut v = if t.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.panel_fill = c32(t.background);
    v.window_fill = c32(t.surface_container);
    v.extreme_bg_color = c32(t.surface_container_low);
    v.faint_bg_color = c32(t.surface_container);
    v.override_text_color = Some(c32(t.on_surface));
    v.hyperlink_color = c32(t.primary);
    v.selection.bg_fill = c32(t.secondary_container);
    v.selection.stroke = Stroke::new(1.0, c32(t.on_secondary_container));
    v.window_corner_radius = CornerRadius::same(18);
    v.window_stroke = Stroke::new(1.0, c32(t.outline_variant));
    let w = &mut v.widgets;
    for (wv, bg) in [
        (&mut w.noninteractive, t.surface_container),
        (&mut w.inactive, t.surface_container_high),
        (&mut w.hovered, t.surface_container_highest),
        (&mut w.active, t.secondary_container),
        (&mut w.open, t.surface_container_highest),
    ] {
        wv.bg_fill = c32(bg);
        wv.weak_bg_fill = c32(bg);
        wv.corner_radius = CornerRadius::same(10);
    }
    w.noninteractive.bg_stroke = Stroke::new(1.0, c32(t.outline_variant));
    w.inactive.bg_stroke = Stroke::NONE;
    w.hovered.bg_stroke = Stroke::new(1.0, c32(t.outline));
    w.active.bg_stroke = Stroke::new(1.0, c32(t.primary));
    w.inactive.fg_stroke = Stroke::new(1.0, c32(t.on_surface));
    w.hovered.fg_stroke = Stroke::new(1.5, c32(t.on_surface));
    w.active.fg_stroke = Stroke::new(1.5, c32(t.on_secondary_container));

    ctx.set_visuals_of(egui::Theme::Dark, v.clone());
    ctx.set_visuals_of(egui::Theme::Light, v);
    ctx.set_theme(if t.dark { egui::Theme::Dark } else { egui::Theme::Light });
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = vec2(8.0, 8.0);
        s.spacing.button_padding = vec2(12.0, 6.0);
        s.spacing.interact_size.y = 30.0;
    });
}

impl eframe::App for Editor {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_files(&ctx);
        self.poll_live();
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::S)) {
            self.save();
        }

        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        match self.tab {
            Tab::Keys => {
                egui::Panel::right("inspector").resizable(false).default_size(400.0).show(ui, |ui| self.inspector(ui));
                egui::CentralPanel::default_margins().show(ui, |ui| self.deck_view(ui));
            }
            Tab::Store => {
                egui::Panel::left("store-categories").resizable(false).default_size(210.0).show(ui, |ui| self.store_categories(ui));
                egui::CentralPanel::default_margins().show(ui, |ui| self.store(ui));
            }
            Tab::Settings => {
                egui::CentralPanel::default_margins().show(ui, |ui| self.settings(ui));
            }
        }
        self.icon_picker(&ctx);
        self.intro_window(&ctx);

        if self.autosave && self.dirty() && self.last_edit.is_none_or(|t| t.elapsed() > AUTOSAVE_DELAY) {
            self.save();
        }
        // Tiene solo le texture usate in questo frame.
        let used = std::mem::take(&mut self.used);
        self.textures.retain(|k, _| used.contains(k));
        ctx.request_repaint_after(Duration::from_millis(400));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;

    /// Salva un frame come PPM se è impostata CYD_DECK_SNAPSHOT=<cartella>.
    fn snapshot(h: &mut Harness<'_, Editor>, name: &str) {
        let img = h.render().expect("render");
        let Some(dir) = std::env::var_os("CYD_DECK_SNAPSHOT") else { return };
        let mut out = format!("P6\n{} {}\n255\n", img.width(), img.height()).into_bytes();
        out.extend(img.as_raw().chunks(4).flat_map(|p| [p[0], p[1], p[2]]));
        std::fs::write(Path::new(&dir).join(format!("{name}.ppm")), out).unwrap();
    }

    /// Editor in inglese: guida rapida passo per passo, scelta del tema e impostazioni.
    #[test]
    fn english_intro_and_themes() {
        use egui_kittest::kittest::Queryable;
        crate::i18n::TEST_ENGLISH.with(|c| c.set(Some(true)));
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("default-config.toml");
        let mut h = Harness::builder()
            .with_size([1180.0, 860.0])
            .wgpu()
            .build_eframe(|cc| Editor::new(cc, path.clone()).unwrap());
        h.state_mut().autosave = false;
        h.state_mut().intro = Some(0);
        h.state_mut().intro_check = Some((Some("/dev/ttyACM0".into()), true));
        h.run_steps(3);
        snapshot(&mut h, "intro-1");
        h.get_by_label("Next").click();
        h.run_steps(3);
        snapshot(&mut h, "intro-2");
        h.get_by_label("Next").click();
        h.run_steps(3);
        // Senza Caelestia nel test il tema si sceglie qui: un pallino cambia i colori.
        h.state_mut().cfg.theme = config::ThemeCfg { style: "custom".into(), accent: "#7cc4ff".into(), light: false };
        h.run_steps(3);
        snapshot(&mut h, "intro-3");
        h.get_by_label("Next").click();
        h.run_steps(3);
        snapshot(&mut h, "intro-4");
        h.get_by_label("Next").click();
        h.run_steps(3);
        assert_eq!(h.state().cfg.pages[0].name, if h.state().scheme_path.exists() { "Caelestia" } else { "Media" });
        assert!(h.state().cfg.pages.iter().flat_map(|p| &p.keys).any(|k| k.label.as_deref() == Some("Mute")), "etichette tradotte");
        snapshot(&mut h, "intro-5");
        h.get_by_label("Get started").click();
        h.run_steps(3);
        assert!(h.state().intro.is_none() && h.state().cfg.intro_done);
        snapshot(&mut h, "en-keys");

        h.state_mut().cfg.theme.light = true;
        h.state_mut().cfg.theme.accent = "#8fd694".into();
        h.state_mut().ha_token_saved = Some(false);
        h.state_mut().keyring.lock().unwrap().status = Some(secrets::Status::Missing);
        h.state_mut().bambu_code_saved = Some(false);
        h.state_mut().tab = Tab::Settings;
        h.run_steps(3);
        let ctx = h.ctx.clone();
        h.state_mut().refresh_theme(&ctx);
        h.run_steps(3);
        snapshot(&mut h, "en-settings-light");
    }

    #[test]
    fn renders_editor() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("default-config.toml");
        let mut h = Harness::builder()
            .with_size([1180.0, 1000.0])
            .wgpu()
            .build_eframe(|cc| Editor::new(cc, path.clone()).unwrap());
        h.state_mut().autosave = false;
        h.state_mut().selected = Some(1);
        h.run_steps(4);
        snapshot(&mut h, "keys");

        // Senza toccare il portachiavi vero.
        h.state_mut().ha_token_saved = Some(false);
        h.state_mut().keyring.lock().unwrap().status = Some(secrets::Status::Missing);
        h.state_mut().bambu_code_saved = Some(false);
        h.state_mut().cfg.homeassistant.url = "http://homeassistant.local:8123".into();
        h.state_mut().saved.homeassistant.url = "http://homeassistant.local:8123".into();
        h.state_mut().tab = Tab::Settings;
        h.run_steps(3);
        snapshot(&mut h, "settings");

        h.state_mut().tab = Tab::Keys;
        h.state_mut().page = 1;
        h.state_mut().selected = Some(5);
        h.state_mut().picker = Some(IconTarget::Normal);
        h.state_mut().icon_search = "light".into();
        h.run_steps(4);
        snapshot(&mut h, "picker");
        assert!(!h.state().dirty(), "la sola visualizzazione non deve modificare la config");

        // L'esempio richiesto: workspace 1 con SKlauncher, workspace 2 con Spotify e game mode.
        h.state_mut().picker = None;
        h.state_mut().page = 0;
        h.state_mut().selected = Some(0);
        h.state_mut().cfg.pages[0].keys[0] = Key {
            icon: Some("rocket_launch".into()),
            label: Some("Gioco".into()),
            step_delay: 300,
            steps: vec![
                Step::CloseWindows { scope: CloseScope::All, workspace: None, special: false },
                Step::Exec { exec: "sklauncher".into(), workspace: Some("1".into()), background: false, wait: false },
                Step::Exec { exec: "spotify".into(), workspace: Some("2".into()), background: false, wait: false },
                Step::Exec { exec: "caelestia shell gameMode enable".into(), workspace: None, background: false, wait: false },
                Step::Workspace { workspace: "1".into() },
            ],
            ..Key::default()
        };
        h.run_steps(4);
        snapshot(&mut h, "macro");
    }

    #[test]
    fn store_adds_to_first_free_key() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("default-config.toml");
        let mut h = Harness::builder()
            .with_size([1180.0, 800.0])
            .wgpu()
            .build_eframe(|cc| Editor::new(cc, path.clone()).unwrap());
        h.state_mut().autosave = false;
        h.state_mut().tab = Tab::Store;
        h.run_steps(3);
        snapshot(&mut h, "store");
        h.state_mut().store_search = "workspace 3".into();
        h.run_steps(3);
        snapshot(&mut h, "store-search");

        let st = h.state_mut();
        let page = st.store_page;
        st.cfg.pages[page].keys.truncate(2);
        let p = st.catalog.iter().find(|p| p.name == "Workspace 3").unwrap().clone();
        st.store_add(&p);
        assert_eq!(st.cfg.pages[page].keys[2].label.as_deref(), Some("WS 3"));
        assert_eq!(st.selected, Some(2));
        // Pagina piena: non sovrascrive niente.
        let full = vec![Key { label: Some("x".into()), ..Key::default() }; st.slots()];
        st.cfg.pages[page].keys = full.clone();
        st.store_add(&p);
        assert_eq!(st.cfg.pages[page].keys, full);
        assert!(st.status.as_ref().is_some_and(|s| s.1));
    }

    #[test]
    fn app_search_keeps_menu_open() {
        use egui_kittest::kittest::Queryable;
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("default-config.toml");
        let mut h = Harness::builder()
            .with_size([1180.0, 900.0])
            .wgpu()
            .build_eframe(|cc| Editor::new(cc, path.clone()).unwrap());
        h.state_mut().autosave = false;
        h.state_mut().selected = Some(0);
        h.state_mut().cfg.pages[0].keys[0].steps = vec![Step::Delay { ms: 100 }];
        h.run_steps(3);
        h.get_by_label("+ Aggiungi passo").click();
        h.run_steps(3);
        h.get_by_label("Apri un'app").click();
        h.run_steps(3);
        h.get_by_label("Cerca app").click();
        h.run_steps(4);
        assert!(h.query_by_label("Cerca app").is_some(), "il menu si è chiuso cliccando la ricerca");
        assert!(h.query_by_label("Apri un'app").is_some());
    }

    #[test]
    fn mixer_page() {
        use egui_kittest::kittest::Queryable;
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("default-config.toml");
        let mut h = Harness::builder()
            .with_size([1180.0, 900.0])
            .wgpu()
            .build_eframe(|cc| Editor::new(cc, path.clone()).unwrap());
        h.state_mut().autosave = false;
        let audio = h.state().cfg.pages.iter().position(|p| p.kind == PageKind::Mixer).expect("pagina audio nella config di esempio");
        // Dati finti: l'anteprima non deve dipendere dall'audio della macchina.
        h.state_mut().live_next = Instant::now() + Duration::from_secs(3600);
        *h.state().live.lock().unwrap() = MixerLive {
            channels: vec![
                Channel { key: "@sink".into(), name: "Generale".into(), icon: "headphones", volume: 0.4, muted: false, target: Target::Sink },
                Channel { key: "app:spotify".into(), name: "Spotify".into(), icon: "music_note", volume: 0.8, muted: false, target: Target::Nodes(vec![1]) },
                Channel { key: "app:firefox".into(), name: "Firefox".into(), icon: "public", volume: 0.3, muted: true, target: Target::Nodes(vec![2]) },
            ],
            np: Some(NowPlaying { player: "spotify".into(), title: "Brano".into(), artist: "Artista".into(), playing: true, position: 30.0, length: 120.0, ..NowPlaying::default() }),
            version: 1,
            ..MixerLive::default()
        };
        h.state_mut().page = audio;
        h.run_steps(4);
        snapshot(&mut h, "mixer");
        assert!(!h.state().dirty(), "la sola visualizzazione non deve modificare la config");

        // Nascondere un'app dal pannello la aggiunge a `hide`.
        h.get_by_label("Firefox").click();
        h.run_steps(2);
        assert_eq!(h.state().cfg.pages[audio].mixer.hide, vec!["Firefox".to_string()]);

        // "+ Pagina" -> pagina audio.
        h.get_by_label("+ Pagina").click();
        h.run_steps(2);
        h.get_by_label("Pagina audio (volumi e musica)").click();
        h.run_steps(2);
        let last = h.state().cfg.pages.last().unwrap();
        assert_eq!((last.kind, last.keys.len()), (PageKind::Mixer, 0));
        assert_ne!(last.name, "Audio", "il nome non deve duplicare quello di una pagina esistente");
    }

    #[test]
    fn widgets_in_grid() {
        use egui_kittest::kittest::Queryable;
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("default-config.toml");
        let mut h = Harness::builder()
            .with_size([1180.0, 900.0])
            .wgpu()
            .build_eframe(|cc| Editor::new(cc, path.clone()).unwrap());
        h.state_mut().autosave = false;
        h.state_mut().live_next = Instant::now() + Duration::from_secs(3600);
        *h.state().live.lock().unwrap() = MixerLive {
            channels: vec![
                Channel { key: "@sink".into(), name: "Generale".into(), icon: "headphones", volume: 0.4, muted: false, target: Target::Sink },
                Channel { key: "app:spotify".into(), name: "Spotify".into(), icon: "music_note", volume: 0.8, muted: false, target: Target::Nodes(vec![1]) },
            ],
            np: Some(NowPlaying { player: "spotify".into(), title: "Brano".into(), artist: "Artista".into(), playing: true, position: 30.0, length: 120.0, ..NowPlaying::default() }),
            version: 1,
            ..MixerLive::default()
        };
        h.state_mut().page = 0;
        h.state_mut().selected = Some(0);
        h.run_steps(2);
        // Il primo tasto diventa un cursore del volume: alto quanto la griglia.
        h.get_by_label("Cursore volume").click();
        h.run_steps(3);
        let k = h.state().cfg.pages[0].keys[0].clone();
        assert_eq!((k.widget, k.w, k.h), (Some(Widget::Volume), 1, 3));
        // Il lettore nella seconda colonna, largo 3.
        h.state_mut().selected = Some(1);
        h.run_steps(2);
        h.get_by_label("Lettore musicale").click();
        h.run_steps(3);
        let k = h.state().cfg.pages[0].keys[1].clone();
        assert_eq!((k.widget, k.w, k.h), (Some(Widget::Player), 3, 1));
        // Il cursore copre i posti 4 e 8: la loro area è quella del cursore.
        let r = h.state().slot_rects.clone();
        assert_eq!(r[0].height() > r[1].height() * 2.5, true);
        h.state_mut().selected = Some(5);
        h.run_steps(4);
        snapshot(&mut h, "widgets");
    }

    #[test]
    fn drag_swaps_keys() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("default-config.toml");
        let mut h = Harness::builder()
            .with_size([1180.0, 720.0])
            .wgpu()
            .build_eframe(|cc| Editor::new(cc, path.clone()).unwrap());
        h.state_mut().autosave = false;
        h.run_steps(3);
        let label = |h: &Harness<'_, Editor>, i: usize| h.state().cfg.pages[0].keys[i].label.clone();
        let (a, b) = (label(&h, 0), label(&h, 1));
        let (p0, p1) = (h.state().slot_rects[0].center(), h.state().slot_rects[1].center());
        h.drag_at(p0);
        h.run_steps(2);
        for t in 1..=8 {
            h.hover_at(p0 + (p1 - p0) * (t as f32 / 8.0));
            h.run_steps(1);
        }
        h.drop_at(p1);
        h.run_steps(3);
        assert_eq!(label(&h, 0), b);
        assert_eq!(label(&h, 1), a);
        assert_eq!(h.state().selected, Some(1));
        assert!(h.state().dirty());
    }
}
