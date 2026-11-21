//! Parti speciali dentro la griglia dei tasti: cursore del volume, lettore musicale e orologio.
//! Occupano uno o più posti della griglia e si mescolano ai tasti normali.

use crate::audio::{self, Channel, Target};
use crate::claude::{self, Limits};
use crate::media::NowPlaying;
use crate::printer;
use crate::mixer::{fmt_time, Button, Hit, Rect, ART};
use crate::render::{Canvas, Fonts, Rgb};
use crate::theme::Theme;

/// Indice del canale a cui si riferisce un cursore.
pub fn find(channels: &[Channel], target: &str) -> Option<usize> {
    let t = target.trim();
    channels.iter().position(|c| match &c.target {
        Target::Sink => t.is_empty() || t == "@sink",
        Target::Source => t == "@mic",
        Target::Nodes(_) => !t.is_empty() && !t.starts_with('@') && c.name.eq_ignore_ascii_case(t),
    })
}

pub fn target_name(target: &str) -> String {
    match target.trim() {
        "" | "@sink" => "Generale".into(),
        "@mic" => "Microfono".into(),
        t => t.into(),
    }
}

fn target_icon(target: &str) -> &'static str {
    match target.trim() {
        "" | "@sink" => "volume_up",
        "@mic" => "mic",
        t => audio::app_icon(t, ""),
    }
}

/// Un cursore più alto che largo si muove in verticale, altrimenti in orizzontale.
pub fn vertical(r: Rect) -> bool {
    r.h >= r.w
}

/// Volume corrispondente a un punto del cursore (a passi dell'1%).
pub fn value_at(r: Rect, x: i32, y: i32) -> f32 {
    // Un po' di margine: alle estremità si arriva comunque a 0% e 100%.
    let f = if vertical(r) {
        ((r.y + r.h) as f32 - 8.0 - y as f32) / (r.h as f32 - 16.0)
    } else {
        (x as f32 - r.x as f32 - 8.0) / (r.w as f32 - 16.0)
    };
    (f.clamp(0.0, 1.0) * 100.0).round() / 100.0
}

/// Cursore del volume a tutta cella, riempito dal basso (o da sinistra) come l'OSD di Caelestia.
/// `ch` è None se l'app non sta suonando.
pub fn draw_volume(c: &mut Canvas, fonts: &Fonts, t: &Theme, r: Rect, radius: f32, ch: Option<&Channel>, target: &str, dragging: bool) {
    c.fill_rect(r.x, r.y, r.w, r.h, t.background);
    let (x, y, w, h) = (r.x as f32, r.y as f32, r.w as f32, r.h as f32);
    let track = if dragging { t.surface_container_highest } else { t.surface_container };
    c.rounded_rect(x, y, w, h, radius, track);
    let name = ch.map_or_else(|| target_name(target), |c| if matches!(c.target, Target::Nodes(_)) { c.name.clone() } else { target_name(target) });
    let vol = ch.map_or(0.0, |c| c.volume.clamp(0.0, 1.0));
    let muted = ch.is_some_and(|c| c.muted);
    let (fill, on_fill) = if muted { (t.outline, t.surface_container) } else { (t.primary, t.on_primary) };
    let icon = match ch {
        Some(c) if c.muted => if matches!(c.target, Target::Source) { "mic_off" } else { "volume_off" },
        Some(c) => c.icon,
        None => target_icon(target),
    };
    let pct = ch.map_or("–".to_string(), |c| format!("{}%", (c.volume * 100.0).round() as i32));
    let off = if ch.is_none() { t.outline } else { t.on_surface_variant };

    if vertical(r) {
        let fh = h * vol;
        if fh >= 1.0 {
            c.rounded_rect(x, y + h - fh, w, fh, radius, fill);
        }
        // Un elemento a quota `yy` è sopra il riempimento?
        let on = |yy: f32| yy > y + h - fh;
        let cx = x + w / 2.0;
        fonts.draw_text(c, &pct, cx, y + 20.0, 13.0, w - 6.0, if on(y + 14.0) { on_fill } else { off });
        let tall = h >= 100.0;
        let icon_cy = if tall { y + h - 34.0 } else { y + h - 19.0 };
        let icon_col = if on(icon_cy) { on_fill } else if ch.is_none() || muted { t.outline } else { t.primary };
        fonts.draw_icon(c, icon, true, cx, icon_cy, 22.0, icon_col);
        if tall {
            fonts.draw_text(c, &name, cx, y + h - 10.0, 11.0, w - 8.0, if on(y + h - 14.0) { on_fill } else { off });
        }
    } else {
        let fw = w * vol;
        if fw >= 1.0 {
            c.rounded_rect(x, y, fw, h, radius, fill);
        }
        let on = |xx: f32| xx < x + fw;
        let cy = y + h / 2.0;
        let icon_col = if on(x + 20.0) { on_fill } else if ch.is_none() || muted { t.outline } else { t.primary };
        fonts.draw_icon(c, icon, true, x + 22.0, cy, 22.0, icon_col);
        let pct_w = fonts.text_width(&pct, 13.0);
        fonts.draw_text_right(c, &pct, x + w - 10.0, cy + 4.5, 13.0, 48.0, if on(x + w - 10.0 - pct_w / 2.0) { on_fill } else { off });
        let name_x = x + 40.0;
        let max_w = w - 40.0 - pct_w - 18.0;
        if max_w > 20.0 {
            fonts.draw_text_left(c, &name, name_x, cy + 4.5, 12.0, max_w, if on(name_x + 10.0) { on_fill } else { off });
        }
    }
}

/// Orologio: ora grande e, se c'è spazio, la data sotto.
pub fn draw_clock(c: &mut Canvas, fonts: &Fonts, t: &Theme, r: Rect, radius: f32, time: &str, date: &str) {
    c.fill_rect(r.x, r.y, r.w, r.h, t.background);
    let (x, y, w, h) = (r.x as f32, r.y as f32, r.w as f32, r.h as f32);
    c.rounded_rect(x, y, w, h, radius, t.surface_container);
    let with_date = h >= 56.0 && !date.is_empty();
    // L'ora occupa la larghezza disponibile senza superare l'altezza.
    let size = (w * 0.34).min(h * if with_date { 0.42 } else { 0.55 }).max(10.0);
    let date_size = (size * 0.42).clamp(10.0, 18.0);
    let block = size * 0.72 + if with_date { date_size + 6.0 } else { 0.0 };
    let base = y + (h - block) / 2.0 + size * 0.72;
    fonts.draw_text(c, time, x + w / 2.0, base, size, w - 8.0, t.primary);
    if with_date {
        fonts.draw_text(c, date, x + w / 2.0, base + date_size + 6.0, date_size, w - 8.0, t.on_surface_variant);
    }
}

/// Limiti di Claude: percentuale usata e tempo all'azzeramento della sessione (5 ore) e della settimana.
/// Icona dei limiti di Claude (Material Symbols).
const LIMITS_ICON: &str = "data_usage";

pub fn draw_limits(c: &mut Canvas, fonts: &Fonts, t: &Theme, r: Rect, radius: f32, limits: Option<&Limits>, now: i64) {
    c.fill_rect(r.x, r.y, r.w, r.h, t.background);
    let (x, y, w, h) = (r.x as f32, r.y as f32, r.w as f32, r.h as f32);
    c.rounded_rect(x, y, w, h, radius, t.surface_container);
    let bar_col = |used: f32| if used >= 90.0 { t.error } else { t.primary };
    let bar = |c: &mut Canvas, bx: f32, by: f32, bw: f32, used: f32| {
        c.rounded_rect(bx, by, bw, 6.0, 3.0, t.surface_container_highest);
        if used > 0.0 {
            c.rounded_rect(bx, by, (bw * used / 100.0).clamp(6.0, bw), 6.0, 3.0, bar_col(used));
        }
    };
    let Some(l) = limits else {
        fonts.draw_icon(c, LIMITS_ICON, true, x + w / 2.0, y + h / 2.0 - 10.0, (h * 0.4).min(40.0), t.primary);
        fonts.draw_text(c, tr!("Apri Claude Code"), x + w / 2.0, y + h / 2.0 + 20.0, 11.0, w - 8.0, t.on_surface_variant);
        return;
    };
    let five = l.five_hour.map(|f| (f.used_now(now), f.resets_at));
    let week = l.seven_day.map(|f| (f.used_now(now), f.resets_at));

    if w < 110.0 {
        // Un tasto: solo la sessione, in grande.
        let (used, _) = five.or(week).unwrap_or((0.0, 0));
        fonts.draw_icon(c, LIMITS_ICON, true, x + w / 2.0, y + 14.0, 18.0, t.primary);
        fonts.draw_text(c, &format!("{used:.0}%"), x + w / 2.0, y + h / 2.0 + 10.0, 20.0, w - 6.0, bar_col(used));
        let what = if five.is_some() { tr!("5 ore") } else { tr!("settimana") };
        fonts.draw_text(c, what, x + w / 2.0, y + h - 8.0, 10.0, w - 6.0, t.on_surface_variant);
        return;
    }

    let rows: Vec<(&str, f32, i64)> = [(tr!("Sessione (5 ore)"), five), (tr!("Settimana"), week)]
        .into_iter()
        .filter_map(|(n, v)| v.map(|(u, r)| (n, u, r)))
        .collect();
    let (pad, inner) = (12.0, w - 24.0);
    if h < 100.0 {
        // Una riga di tasti: sessione con la barra, settimana in piccolo.
        let Some(&(_, used, reset)) = rows.first() else { return };
        fonts.draw_icon(c, LIMITS_ICON, true, x + pad + 10.0, y + 18.0, 18.0, t.primary);
        fonts.draw_text_left(c, &format!("{used:.0}%"), x + pad + 26.0, y + 24.0, 16.0, 60.0, bar_col(used));
        let right = match week.filter(|_| five.is_some()) {
            Some((wu, _)) => tr!("sett. {}%", format!("{wu:.0}")),
            None => String::new(),
        };
        fonts.draw_text_right(c, &right, x + w - pad, y + 23.0, 11.0, inner / 2.0, t.on_surface_variant);
        bar(c, x + pad, y + 32.0, inner, used);
        let when = if used == 0.0 && reset <= now { tr!("nuova finestra").to_string() } else { tr!("si azzera tra {}", claude::until(reset, now)) };
        fonts.draw_text_left(c, &when, x + pad, y + 52.0, 10.0, inner, t.on_surface_variant);
        return;
    }

    // Intestazione e una sezione per finestra.
    fonts.draw_icon(c, LIMITS_ICON, true, x + pad + 12.0, y + 20.0, 22.0, t.primary);
    fonts.draw_text_left(c, tr!("Limiti Claude"), x + pad + 32.0, y + 25.0, 14.0, inner - 90.0, t.on_surface);
    let age = (now - l.updated).max(0) / 60;
    let age = if age < 1 { tr!("adesso").to_string() } else if age < 60 { tr!("{} min fa", age) } else { tr!("{} h fa", age / 60) };
    fonts.draw_text_right(c, &age, x + w - pad, y + 24.0, 10.0, 80.0, t.outline);
    let top = y + 40.0;
    let sec_h = (h - 40.0 - 8.0) / rows.len().max(1) as f32;
    for (i, (name, used, reset)) in rows.iter().enumerate() {
        let sy = top + i as f32 * sec_h;
        // Il blocco (nome, barra, azzeramento) è alto circa 42 px: centrato nella sua parte.
        let by = sy + ((sec_h - 42.0) / 2.0).max(0.0);
        fonts.draw_text_left(c, name, x + pad, by + 12.0, 12.0, inner - 50.0, t.on_surface_variant);
        fonts.draw_text_right(c, &format!("{used:.0}%"), x + w - pad, by + 13.0, 15.0, 50.0, bar_col(*used));
        bar(c, x + pad, by + 19.0, inner, *used);
        let when = if *reset <= now { tr!("nuova finestra").to_string() } else { tr!("si azzera tra {}", claude::until(*reset, now)) };
        fonts.draw_text_left(c, &when, x + pad, by + 40.0, 10.0, inner, t.outline);
    }
}

/// Stampante 3D: barra di avanzamento con percentuale, tempo rimanente, strato e temperature.
pub fn draw_printer(c: &mut Canvas, fonts: &Fonts, t: &Theme, r: Rect, radius: f32, st: Option<&printer::Status>) {
    use printer::Cmd;
    c.fill_rect(r.x, r.y, r.w, r.h, t.background);
    let (x, y, w, h) = (r.x as f32, r.y as f32, r.w as f32, r.h as f32);
    c.rounded_rect(x, y, w, h, radius, t.surface_container);
    let (pad, inner) = (12.0, w - 24.0);
    let Some(st) = st.filter(|s| s.online) else {
        fonts.draw_icon(c, "print_disabled", false, x + w / 2.0, y + h / 2.0 - 8.0, (h * 0.35).min(32.0), t.outline);
        fonts.draw_text(c, tr!("Stampante offline"), x + w / 2.0, y + h / 2.0 + 18.0, 11.0, w - 8.0, t.on_surface_variant);
        return;
    };
    let failed = st.gcode_state() == "FAILED";
    let accent = if failed { t.error } else { t.primary };
    let active = st.active();
    let pct = st.percent() as f32;
    let bar = |c: &mut Canvas, by: f32, bh: f32| {
        c.rounded_rect(x + pad, by, inner, bh, bh / 2.0, t.surface_container_highest);
        if pct > 0.0 {
            c.rounded_rect(x + pad, by, (inner * pct / 100.0).clamp(bh, inner), bh, bh / 2.0, if st.paused() { t.outline } else { accent });
        }
    };
    let big = if active { format!("{}%", st.percent()) } else { st.state_it().to_string() };
    let icon = match st.gcode_state() {
        "PAUSE" => "pause_circle",
        "FINISH" => "check_circle",
        "FAILED" => "error",
        _ => "print",
    };

    if w < 110.0 {
        // Un tasto: percentuale (o stato) e una barretta in fondo.
        fonts.draw_icon(c, icon, false, x + w / 2.0, y + 16.0, 18.0, accent);
        fonts.draw_text(c, &big, x + w / 2.0, y + h / 2.0 + 8.0, if active { 20.0 } else { 13.0 }, w - 6.0, t.on_surface);
        if active {
            let (bx, bw) = (x + 8.0, w - 16.0);
            c.rounded_rect(bx, y + h - 14.0, bw, 5.0, 2.5, t.surface_container_highest);
            c.rounded_rect(bx, y + h - 14.0, (bw * pct / 100.0).clamp(5.0, bw), 5.0, 2.5, accent);
        }
        return;
    }

    let file = st.answer(Cmd::File);
    let temps = st.answer(Cmd::Temps);
    if h < 100.0 {
        // Una riga di tasti: icona e percentuale, tempo a destra, barra, dettagli sotto.
        fonts.draw_icon(c, icon, false, x + pad + 10.0, y + 18.0, 20.0, accent);
        fonts.draw_text_left(c, &big, x + pad + 28.0, y + 25.0, 16.0, inner / 2.0, t.on_surface);
        let right = if active { st.remaining() } else { temps.clone() };
        fonts.draw_text_right(c, &right, x + w - pad, y + 24.0, 12.0, inner / 2.0, t.on_surface_variant);
        if active {
            bar(c, y + 33.0, 8.0);
            let detail = tr!("strato {}  ·  {}", st.answer(Cmd::Layer), temps);
            fonts.draw_text_left(c, &file, x + pad, y + 56.0, 10.0, inner * 0.45, t.outline);
            fonts.draw_text_right(c, &detail, x + w - pad, y + 56.0, 10.0, inner * 0.55, t.on_surface_variant);
        } else if !file.is_empty() {
            fonts.draw_text_left(c, &file, x + pad, y + 52.0, 11.0, inner, t.outline);
        }
        return;
    }

    // Grande: intestazione, nome del file, percentuale grande, barra spessa e tre valori sotto.
    fonts.draw_icon(c, icon, false, x + pad + 11.0, y + 20.0, 22.0, accent);
    fonts.draw_text_left(c, tr!("Stampante 3D"), x + pad + 30.0, y + 25.0, 14.0, inner * 0.55, t.on_surface);
    fonts.draw_text_right(c, st.state_it(), x + w - pad, y + 24.0, 11.0, inner * 0.4, accent);
    let mid = y + h / 2.0;
    if active {
        fonts.draw_text_left(c, &file, x + pad, mid - 16.0, 12.0, inner * 0.65, t.on_surface_variant);
        fonts.draw_text_right(c, &big, x + w - pad, mid - 12.0, 24.0, inner * 0.35, t.on_surface);
        bar(c, mid - 2.0, 12.0);
        let cols = [(tr!("rimanente"), st.remaining()), (tr!("strato"), st.answer(Cmd::Layer)), (tr!("ugello / piatto"), temps)];
        let cw = inner / 3.0;
        for (k, (label, value)) in cols.iter().enumerate() {
            let cx = x + pad + cw * (k as f32 + 0.5);
            fonts.draw_text(c, value, cx, mid + 32.0, 14.0, cw - 4.0, t.on_surface);
            fonts.draw_text(c, label, cx, mid + 47.0, 9.0, cw - 4.0, t.outline);
        }
    } else {
        fonts.draw_text(c, &temps, x + w / 2.0, mid + 10.0, 18.0, inner, t.on_surface);
        fonts.draw_text(c, tr!("ugello / piatto"), x + w / 2.0, mid + 26.0, 9.0, inner, t.outline);
        if !file.is_empty() {
            fonts.draw_text(c, &tr!("ultima: {}", file), x + w / 2.0, y + h - 12.0, 10.0, inner, t.on_surface_variant);
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Solo copertina e play/pausa (un tasto o poco più).
    Compact,
    /// Copertina sopra, testi e pulsanti sotto.
    Tall,
    /// Come la scheda della pagina audio: copertina a sinistra, pulsanti a destra.
    Wide,
}

fn mode(r: Rect) -> Mode {
    if r.w < 110 || r.h < 44 {
        Mode::Compact
    } else if r.h >= 120 && (r.w as f32) < r.h as f32 * 1.6 {
        Mode::Tall
    } else {
        Mode::Wide
    }
}

/// Lato della copertina e suo angolo in alto a sinistra.
fn art_box(r: Rect) -> (f32, f32, u32) {
    let (x, y, w, h) = (r.x as f32, r.y as f32, r.w, r.h);
    match mode(r) {
        Mode::Compact => {
            let a = (w.min(h).saturating_sub(16)).min(ART);
            (x + (w - a) as f32 / 2.0, y + (h - a) as f32 / 2.0, a)
        }
        Mode::Tall => {
            // Sotto la copertina servono titolo, artista, (avanzamento) e i pulsanti.
            let a = (w.saturating_sub(16)).min(h.saturating_sub(if h < 160 { 86 } else { 96 })).min(ART);
            (x + (w - a) as f32 / 2.0, y + 8.0, a)
        }
        Mode::Wide => {
            let a = h.saturating_sub(16).min(ART);
            (x + 8.0, y + (h - a) as f32 / 2.0, a)
        }
    }
}

/// Pulsanti (centro x, centro y, raggio).
fn buttons(r: Rect) -> Vec<(Button, f32, f32, f32)> {
    let (x, y, w, h) = (r.x as f32, r.y as f32, r.w as f32, r.h as f32);
    match mode(r) {
        Mode::Compact => vec![(Button::PlayPause, x + w - 15.0, y + h - 15.0, 12.0)],
        Mode::Tall => {
            let cy = y + h - 20.0;
            let cx = x + w / 2.0;
            if w >= 130.0 {
                vec![(Button::Prev, cx - 40.0, cy, 16.0), (Button::PlayPause, cx, cy, 16.0), (Button::Next, cx + 40.0, cy, 16.0)]
            } else {
                vec![(Button::PlayPause, cx, cy, 16.0)]
            }
        }
        Mode::Wide => {
            let cy = y + h / 2.0;
            let right = x + w - 24.0;
            let (_, _, a) = art_box(r);
            // Con poco spazio per i testi resta solo play/pausa.
            if w - a as f32 - 26.0 - 120.0 >= 60.0 {
                vec![(Button::Prev, right - 80.0, cy, 18.0), (Button::PlayPause, right - 40.0, cy, 18.0), (Button::Next, right, cy, 18.0)]
            } else {
                vec![(Button::PlayPause, right, cy, 18.0)]
            }
        }
    }
}

/// Zona dei testi: x, larghezza, baseline di titolo e artista, quota della barra di avanzamento.
struct Text {
    x: f32,
    w: f32,
    title: f32,
    artist: f32,
    progress: Option<f32>,
}

fn text_box(r: Rect) -> Option<Text> {
    match mode(r) {
        Mode::Compact => None,
        Mode::Tall => {
            let (_, ay, a) = art_box(r);
            let title = ay + a as f32 + 16.0;
            let artist = title + 15.0;
            Some(Text { x: r.x as f32 + 8.0, w: r.w as f32 - 16.0, title, artist, progress: (r.h >= 160).then_some(artist + 7.0) })
        }
        Mode::Wide => {
            let (ax, _, a) = art_box(r);
            let x = ax + a as f32 + 10.0;
            let (_, bx, _, br) = buttons(r).first().copied()?;
            let title = (r.y + r.h / 2) as f32 - 6.0;
            let artist = title + 16.0;
            let progress = (artist + 12.0 <= (r.y + r.h) as f32 - 4.0).then_some(artist + 8.0);
            Some(Text { x, w: bx - br - x - 4.0, title, artist, progress })
        }
    }
}

/// Barra di avanzamento: l'unica cosa che cambia ogni secondo mentre suona.
pub fn progress(r: Rect) -> Option<Rect> {
    let t = text_box(r)?;
    Some(Rect { x: t.x as u32, y: t.progress? as u32, w: t.w.max(0.0) as u32 + 1, h: 4 })
}

pub fn player_hit(r: Rect, x: i32, y: i32) -> Hit {
    for (b, cx, cy, rad) in buttons(r) {
        // Area un po' più grande del pulsante: le dita sono imprecise.
        if (x as f32 - cx).abs() <= rad + 4.0 && (y as f32 - cy).abs() <= rad + 8.0 {
            return Hit::Button(b);
        }
    }
    // Su un tasto solo tutto il riquadro fa play/pausa.
    if mode(r) == Mode::Compact { Hit::Button(Button::PlayPause) } else { Hit::Card }
}

/// Riduce la copertina (presa a `ART` pixel) al lato `to`.
fn scaled(img: &[Rgb], to: u32) -> Vec<Rgb> {
    if to == ART {
        return img.to_vec();
    }
    (0..to * to)
        .map(|i| {
            let (x, y) = (i % to, i / to);
            img[((y * ART / to) * ART + x * ART / to) as usize]
        })
        .collect()
}

pub fn draw_player(c: &mut Canvas, fonts: &Fonts, t: &Theme, r: Rect, radius: f32, np: Option<&NowPlaying>, art: Option<&[Rgb]>, pressed: Option<Hit>) {
    c.fill_rect(r.x, r.y, r.w, r.h, t.background);
    let bg = if pressed == Some(Hit::Card) { t.secondary_container } else { t.surface_container };
    c.rounded_rect(r.x as f32, r.y as f32, r.w as f32, r.h as f32, radius, bg);

    let (ax, ay, a) = art_box(r);
    match art.filter(|i| i.len() == (ART * ART) as usize) {
        Some(img) if a > 0 => c.image_rounded(ax, ay, a, 10.0, &scaled(img, a)),
        _ => {
            c.rounded_rect(ax, ay, a as f32, a as f32, 10.0, t.surface_container_highest);
            let icon = if np.is_some() { "music_note" } else { "music_off" };
            fonts.draw_icon(c, icon, true, ax + a as f32 / 2.0, ay + a as f32 / 2.0, a as f32 * 0.5, t.primary);
        }
    }

    if let Some(tb) = text_box(r) {
        let tall = mode(r) == Mode::Tall;
        let line = |c: &mut Canvas, s: &str, base: f32, size: f32, col: Rgb| {
            if tall {
                fonts.draw_text(c, s, tb.x + tb.w / 2.0, base, size, tb.w, col);
            } else {
                fonts.draw_text_left(c, s, tb.x, base, size, tb.w, col);
            }
        };
        match np {
            Some(np) => {
                let title = if np.title.is_empty() { np.player.as_str() } else { np.title.as_str() };
                line(c, title, tb.title, if tall { 13.0 } else { 14.0 }, t.on_surface);
                let sub = if np.artist.is_empty() { fmt_time(np.position) } else { np.artist.clone() };
                line(c, &sub, tb.artist, 11.0, t.on_surface_variant);
                if let (Some(p), true) = (progress(r), np.length > 0.0) {
                    let frac = (np.position / np.length).clamp(0.0, 1.0);
                    let (px, py, pw) = (p.x as f32, p.y as f32, p.w as f32 - 1.0);
                    c.rounded_rect(px, py, pw, 4.0, 2.0, t.surface_container_highest);
                    if frac > 0.0 {
                        c.rounded_rect(px, py, (pw * frac).max(4.0), 4.0, 2.0, t.primary);
                    }
                }
            }
            None => {
                line(c, tr!("Niente in riproduzione"), tb.title, 12.0, t.on_surface);
                line(c, tr!("Tocca per la musica"), tb.artist, 11.0, t.on_surface_variant);
            }
        }
    }

    let has_player = np.is_some();
    for (b, cx, cy, rad) in buttons(r) {
        let pressed = pressed == Some(Hit::Button(b));
        match b {
            Button::PlayPause => {
                let fill = if pressed { t.secondary_container } else if has_player { t.primary } else { t.surface_container_highest };
                c.circle(cx, cy, rad, fill);
                let icon = if np.is_some_and(|n| n.playing) { "pause" } else { "play_arrow" };
                let col = if pressed { t.on_secondary_container } else if has_player { t.on_primary } else { t.outline };
                fonts.draw_icon(c, icon, true, cx, cy, rad * 1.45, col);
            }
            _ => {
                if pressed {
                    c.circle(cx, cy, rad, t.secondary_container);
                }
                let icon = if b == Button::Prev { "skip_previous" } else { "skip_next" };
                let fg = if has_player { t.on_surface_variant } else { t.outline_variant };
                fonts.draw_icon(c, icon, true, cx, cy, 26.0, fg);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_and_player_geometry() {
        let tall = Rect { x: 24, y: 30, w: 62, h: 202 };
        assert!(vertical(tall));
        assert_eq!((value_at(tall, 0, 30), value_at(tall, 0, 232)), (1.0, 0.0));
        let wide = Rect { x: 24, y: 30, w: 132, h: 62 };
        assert!(!vertical(wide));
        assert_eq!(value_at(wide, 24 + 66, 0), 0.5);

        let ch = vec![
            Channel { key: "@sink".into(), name: "Generale".into(), icon: "volume_up", volume: 0.4, muted: false, target: Target::Sink },
            Channel { key: "app:spotify".into(), name: "Spotify".into(), icon: "music_note", volume: 1.0, muted: false, target: Target::Nodes(vec![3]) },
        ];
        assert_eq!((find(&ch, ""), find(&ch, "spotify"), find(&ch, "@mic"), find(&ch, "Firefox")), (Some(0), Some(1), None, None));

        // Lettore su una riga intera: tre pulsanti a destra, la copertina a sinistra è "Card".
        let row = Rect { x: 24, y: 30, w: 272, h: 62 };
        let b = buttons(row);
        assert_eq!(b.len(), 3);
        let (_, px, py, _) = b[1];
        assert_eq!(player_hit(row, px as i32, py as i32), Hit::Button(Button::PlayPause));
        assert_eq!(player_hit(row, 40, 50), Hit::Card);
        assert!(progress(row).is_some_and(|p| p.y + p.h <= row.y + row.h));
        // Su un tasto solo, tutto fa play/pausa.
        let one = Rect { x: 0, y: 0, w: 62, h: 62 };
        assert_eq!(player_hit(one, 10, 10), Hit::Button(Button::PlayPause));
        assert!(progress(one).is_none());
        // 2x2: copertina sopra, pulsanti sotto.
        let sq = Rect { x: 0, y: 0, w: 132, h: 132 };
        assert_eq!(buttons(sq).len(), 3);
        assert!(progress(sq).is_none());
        let t = text_box(sq).unwrap();
        assert!(t.artist + 3.0 <= buttons(sq)[0].2 - 16.0);
        // 2x3: copertina, testi, avanzamento e pulsanti.
        let col = Rect { x: 0, y: 0, w: 132, h: 202 };
        assert_eq!(buttons(col).len(), 3);
        assert!(progress(col).is_some_and(|p| (p.y + p.h) as f32 <= buttons(col)[0].2 - 16.0));
    }
}

#[cfg(test)]
mod printer_tests {
    use super::*;

    /// Disegna il widget della stampante nelle tre misure; con CYD_DECK_SNAPSHOT=<cartella> salva un PPM.
    #[test]
    fn printer_widget_sizes() {
        let cfg: crate::config::Config = toml::from_str(crate::config::DEFAULT_CONFIG).unwrap();
        let fonts = Fonts::load(&cfg.layout.icon_font, &cfg.layout.text_font).unwrap();
        let t = crate::theme::load(std::path::Path::new("/nonexistent"));
        let (w, h) = (320u32, 330u32);
        let mut c = Canvas::new(w, h);
        let mut idle = printer::sample();
        idle.print.insert("gcode_state".into(), serde_json::json!("FINISH"));
        let st = printer::sample();
        draw_printer(&mut c, &fonts, &t, Rect { x: 4, y: 4, w: 72, h: 72 }, 14.0, Some(&st));
        draw_printer(&mut c, &fonts, &t, Rect { x: 84, y: 4, w: 232, h: 72 }, 14.0, Some(&idle));
        draw_printer(&mut c, &fonts, &t, Rect { x: 4, y: 84, w: 312, h: 72 }, 14.0, Some(&st));
        draw_printer(&mut c, &fonts, &t, Rect { x: 4, y: 164, w: 312, h: 152 }, 14.0, Some(&st));
        let Some(dir) = std::env::var_os("CYD_DECK_SNAPSHOT") else { return };
        let rgba = c.rgba();
        let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
        out.extend(rgba.chunks(4).flat_map(|p| [p[0], p[1], p[2]]));
        std::fs::write(std::path::Path::new(&dir).join("printer.ppm"), out).unwrap();
    }
}
