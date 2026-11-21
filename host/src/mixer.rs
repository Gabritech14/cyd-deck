//! Pagina audio: scheda del lettore attivo di Caelestia e un cursore di volume per app.

use crate::audio::{Channel, Target};
use crate::config::MixerConfig;
use crate::media::NowPlaying;
use crate::render::{Canvas, Fonts, Rgb};
use crate::theme::Theme;

const M: u32 = 8;
const CARD_H: u32 = 72;
const ROW_H: u32 = 32;
const GAP: u32 = 4;
const COL_W: u32 = 52;
const COL_GAP: u32 = 11;
/// Spazio sotto il cursore verticale per il nome (toccarlo attiva il muto).
const NAME_H: u32 = 30;
pub const ART: u32 = 56;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Prev,
    PlayPause,
    Next,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// Scheda del lettore, fuori dai pulsanti.
    Card,
    Button(Button),
    /// Icona/nome di una riga (indice del canale).
    Mute(usize),
    Slider(usize),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x as i32 && y >= self.y as i32 && x < (self.x + self.w) as i32 && y < (self.y + self.h) as i32
    }
}

/// Cosa disegnare: lo stato viene da `audio` e `media`, il resto dal tocco in corso.
pub struct View<'a> {
    pub channels: &'a [Channel],
    pub np: Option<&'a NowPlaying>,
    pub art: Option<&'a [Rgb]>,
    pub scroll: usize,
    /// Riga in trascinamento.
    pub dragging: Option<usize>,
    pub pressed: Option<Hit>,
}

#[derive(Debug, Clone, Copy)]
pub struct Layout {
    pub w: u32,
    pub h: u32,
    pub card: Option<Rect>,
    pub rows_y: u32,
    pub visible: usize,
    /// Cursori verticali affiancati invece di righe orizzontali.
    pub vertical: bool,
}

impl Layout {
    /// `top`: spazio occupato in alto dalla barra di stato.
    pub fn new(cfg: &MixerConfig, w: u32, h: u32, top: u32) -> Self {
        let card = cfg.player.then_some(Rect { x: M, y: top + M, w: w - 2 * M, h: CARD_H });
        let rows_y = card.map_or(top + M, |c| c.y + c.h + M);
        let visible = if cfg.vertical {
            (w - 2 * M + COL_GAP) / (COL_W + COL_GAP)
        } else {
            (h.saturating_sub(rows_y + M) + GAP) / (ROW_H + GAP)
        };
        Self { w, h, card, rows_y, visible: visible.max(1) as usize, vertical: cfg.vertical }
    }

    /// Zona sotto la scheda, ridisegnata in blocco quando cambia l'elenco.
    pub fn rows_area(&self) -> Rect {
        Rect { x: 0, y: self.rows_y, w: self.w, h: self.h - self.rows_y }
    }

    pub fn row(&self, slot: usize) -> Rect {
        Rect { x: M, y: self.rows_y + slot as u32 * (ROW_H + GAP), w: self.w - 2 * M, h: ROW_H }
    }

    /// Posto `slot` per uno dei `n` canali: una riga, oppure una colonna
    /// (le colonne sono centrate quando ce ne sono poche).
    pub fn cell(&self, slot: usize, n: usize) -> Rect {
        if !self.vertical {
            return self.row(slot);
        }
        let count = n.clamp(1, self.visible) as u32;
        let x0 = (self.w - (count * COL_W + (count - 1) * COL_GAP)) / 2;
        Rect { x: x0 + slot as u32 * (COL_W + COL_GAP), y: self.rows_y, w: COL_W, h: self.h - self.rows_y - M }
    }

    /// Cursore verticale dentro una colonna: (x, y, larghezza, altezza).
    pub fn pill(&self, col: Rect) -> (f32, f32, f32, f32) {
        ((col.x + 6) as f32, col.y as f32, (col.w - 12) as f32, (col.h - NAME_H) as f32)
    }

    /// Estremi orizzontali della barra del volume.
    pub fn bar(&self) -> (f32, f32) {
        ((M + 112) as f32, (self.w - M - 52) as f32)
    }

    pub fn max_scroll(&self, n: usize) -> usize {
        n.saturating_sub(self.visible)
    }

    pub fn button(&self, b: Button) -> Option<(f32, f32, f32)> {
        let c = self.card?;
        let cy = (c.y + c.h / 2) as f32;
        let right = (c.x + c.w) as f32 - 24.0;
        let cx = match b {
            Button::Next => right,
            Button::PlayPause => right - 40.0,
            Button::Prev => right - 80.0,
        };
        Some((cx, cy, 18.0))
    }

    /// Barra di avanzamento del brano: è l'unica cosa che cambia ogni secondo.
    pub fn progress(&self) -> Option<Rect> {
        let c = self.card?;
        let tx = c.x + 8 + ART + 10;
        let (prev_cx, _, r) = self.button(Button::Prev)?;
        let w = (prev_cx - r - tx as f32 - 4.0) as u32;
        Some(Rect { x: tx, y: c.y + 56, w: w + 1, h: 4 })
    }

    /// Volume corrispondente a un punto del cursore (a passi dell'1%).
    pub fn value_at(&self, x: i32, y: i32) -> f32 {
        let f = if self.vertical {
            let (_, top, _, h) = self.pill(self.cell(0, 1));
            // Un po' di margine: in cima e in fondo si arriva comunque a 100% e 0%.
            (top + h - 6.0 - y as f32) / (h - 12.0)
        } else {
            let (a, b) = self.bar();
            (x as f32 - a) / (b - a)
        };
        (f.clamp(0.0, 1.0) * 100.0).round() / 100.0
    }

    pub fn hit(&self, x: i32, y: i32, scroll: usize, n: usize) -> Option<Hit> {
        if let Some(card) = self.card.filter(|c| c.contains(x, y)) {
            for b in [Button::Prev, Button::PlayPause, Button::Next] {
                let (cx, cy, r) = self.button(b)?;
                // Area un po' più grande del pulsante disegnato: le dita sono imprecise.
                if (x as f32 - cx).abs() <= r + 2.0 && (y as f32 - cy).abs() <= card.h as f32 / 2.0 {
                    return Some(Hit::Button(b));
                }
            }
            return Some(Hit::Card);
        }
        let slot = (0..self.visible).find(|&s| {
            let r = self.cell(s, n);
            // Include metà dello spazio tra le celle.
            if self.vertical {
                Rect { x: r.x.saturating_sub(COL_GAP / 2), w: r.w + COL_GAP, ..r }.contains(x, y)
            } else {
                Rect { y: r.y.saturating_sub(GAP / 2), h: r.h + GAP, ..r }.contains(x, y)
            }
        })?;
        let i = scroll + slot;
        if i >= n {
            return None;
        }
        let slider = if self.vertical {
            let (_, top, _, h) = self.pill(self.cell(slot, n));
            (y as f32) < top + h + 2.0
        } else {
            x as f32 >= self.bar().0 - 16.0
        };
        Some(if slider { Hit::Slider(i) } else { Hit::Mute(i) })
    }
}

fn mute_icon(ch: &Channel) -> &'static str {
    match ch.target {
        Target::Source => "mic_off",
        Target::Sink => "volume_off",
        Target::Nodes(_) => "volume_off",
    }
}

pub fn fmt_time(s: f32) -> String {
    let s = s.max(0.0) as u32;
    format!("{}:{:02}", s / 60, s % 60)
}

pub fn draw_card(c: &mut Canvas, fonts: &Fonts, t: &Theme, l: &Layout, v: &View) {
    let Some(card) = l.card else { return };
    c.fill_rect(card.x, card.y, card.w, card.h, t.background);
    let (x, y) = (card.x as f32, card.y as f32);
    let pressed_card = v.pressed == Some(Hit::Card);
    let bg = if pressed_card { t.secondary_container } else { t.surface_container };
    c.rounded_rect(x, y, card.w as f32, card.h as f32, 18.0, bg);

    let (ax, ay) = (x + 8.0, y + 8.0);
    match v.art.filter(|a| a.len() == (ART * ART) as usize) {
        Some(img) => c.image_rounded(ax, ay, ART, 12.0, img),
        None => {
            c.rounded_rect(ax, ay, ART as f32, ART as f32, 12.0, t.surface_container_highest);
            let icon = if v.np.is_some() { "music_note" } else { "music_off" };
            fonts.draw_icon(c, icon, true, ax + ART as f32 / 2.0, ay + ART as f32 / 2.0, 30.0, t.primary);
        }
    }

    let tx = ax + ART as f32 + 10.0;
    let (prev_cx, _, r) = l.button(Button::Prev).unwrap_or_default();
    let max_w = prev_cx - r - tx - 4.0;
    let has_player = v.np.is_some();
    match v.np {
        Some(np) => {
            let title = if np.title.is_empty() { np.player.as_str() } else { np.title.as_str() };
            fonts.draw_text_left(c, title, tx, y + 28.0, 15.0, max_w, t.on_surface);
            let sub = if np.artist.is_empty() { fmt_time(np.position) } else { np.artist.clone() };
            fonts.draw_text_left(c, &sub, tx, y + 46.0, 12.0, max_w, t.on_surface_variant);
            if np.length > 0.0 {
                let frac = (np.position / np.length).clamp(0.0, 1.0);
                let by = y + 56.0;
                c.rounded_rect(tx, by, max_w, 4.0, 2.0, t.surface_container_highest);
                if frac > 0.0 {
                    c.rounded_rect(tx, by, (max_w * frac).max(4.0), 4.0, 2.0, t.primary);
                }
            }
        }
        None => {
            fonts.draw_text_left(c, tr!("Niente in riproduzione"), tx, y + 32.0, 14.0, max_w, t.on_surface);
            fonts.draw_text_left(c, tr!("Tocca per la musica"), tx, y + 50.0, 12.0, max_w, t.on_surface_variant);
        }
    }

    for b in [Button::Prev, Button::PlayPause, Button::Next] {
        let Some((cx, cy, r)) = l.button(b) else { continue };
        let pressed = v.pressed == Some(Hit::Button(b));
        let fg = if has_player { t.on_surface_variant } else { t.outline_variant };
        match b {
            Button::PlayPause => {
                let fill = if pressed { t.secondary_container } else if has_player { t.primary } else { t.surface_container_highest };
                c.circle(cx, cy, r, fill);
                let icon = if v.np.is_some_and(|n| n.playing) { "pause" } else { "play_arrow" };
                let col = if pressed { t.on_secondary_container } else if has_player { t.on_primary } else { t.outline };
                fonts.draw_icon(c, icon, true, cx, cy, 26.0, col);
            }
            _ => {
                if pressed {
                    c.circle(cx, cy, r, t.secondary_container);
                }
                let icon = if b == Button::Prev { "skip_previous" } else { "skip_next" };
                fonts.draw_icon(c, icon, true, cx, cy, 26.0, fg);
            }
        }
    }
}

/// Disegna il cursore verticale nella colonna `slot`.
fn draw_column(c: &mut Canvas, fonts: &Fonts, t: &Theme, l: &Layout, v: &View, slot: usize) {
    let r = l.cell(slot, v.channels.len());
    c.fill_rect(r.x, r.y, r.w, r.h, t.background);
    let i = v.scroll + slot;
    let Some(ch) = v.channels.get(i) else { return };
    let (px, py, pw, ph) = l.pill(r);
    let drag = v.dragging == Some(i);
    let radius = 14.0;
    c.rounded_rect(px, py, pw, ph, radius, if drag { t.surface_container_highest } else { t.surface_container_high });
    // Il riempimento sale dal basso come nei cursori dell'OSD di Caelestia.
    let fh = ph * ch.volume.clamp(0.0, 1.0);
    let (fill, on_fill) = if ch.muted { (t.outline, t.surface_container) } else { (t.primary, t.on_primary) };
    if fh >= 1.0 {
        c.rounded_rect(px, py + ph - fh, pw, fh, radius, fill);
    }
    let cx = px + pw / 2.0;
    let icon_cy = py + ph - 18.0;
    let icon = if ch.muted { mute_icon(ch) } else { ch.icon };
    let icon_col = if fh >= 30.0 { on_fill } else if ch.muted { t.outline } else { t.primary };
    fonts.draw_icon(c, icon, true, cx, icon_cy, 20.0, icon_col);
    let pct = format!("{}", (ch.volume * 100.0).round() as i32);
    let pct_col = if fh >= ph - 8.0 { on_fill } else { t.on_surface_variant };
    fonts.draw_text(c, &pct, cx, py + 19.0, 12.0, pw - 4.0, pct_col);

    let name_top = py + ph + 4.0;
    let pressed = v.pressed == Some(Hit::Mute(i));
    if pressed {
        c.rounded_rect(r.x as f32, name_top, r.w as f32, (r.y + r.h) as f32 - name_top, 8.0, t.secondary_container);
    }
    let name_col = if pressed { t.on_secondary_container } else if ch.muted { t.outline } else { t.on_surface };
    fonts.draw_text(c, &ch.name, cx, name_top + 16.0, 11.0, r.w as f32 - 2.0, name_col);
}

/// Disegna la riga o colonna `slot` (vuota se non c'è un canale).
pub fn draw_row(c: &mut Canvas, fonts: &Fonts, t: &Theme, l: &Layout, v: &View, slot: usize) {
    if l.vertical {
        return draw_column(c, fonts, t, l, v, slot);
    }
    let r = l.row(slot);
    c.fill_rect(r.x, r.y, r.w, r.h, t.background);
    let i = v.scroll + slot;
    let Some(ch) = v.channels.get(i) else { return };
    let (x, y, w, h) = (r.x as f32, r.y as f32, r.w as f32, r.h as f32);
    let cy = y + h / 2.0;
    let pressed = v.pressed == Some(Hit::Mute(i));
    let bg = if pressed { t.secondary_container } else { t.surface_container };
    c.rounded_rect(x, y, w, h, 12.0, bg);

    let (icon, icon_col) = if ch.muted { (mute_icon(ch), t.outline) } else { (ch.icon, t.primary) };
    fonts.draw_icon(c, icon, !ch.muted, x + 18.0, cy, 20.0, icon_col);
    let (a, b) = l.bar();
    let name_col = if ch.muted { t.outline } else { t.on_surface };
    fonts.draw_text_left(c, &ch.name, x + 34.0, cy + 4.5, 13.0, a - 10.0 - (x + 34.0), name_col);

    let drag = v.dragging == Some(i);
    let track_h = if drag { 10.0 } else { 8.0 };
    c.rounded_rect(a, cy - track_h / 2.0, b - a, track_h, track_h / 2.0, t.surface_container_highest);
    let vol = ch.volume.clamp(0.0, 1.0);
    let kx = a + (b - a) * vol;
    let fill = if ch.muted { t.outline } else { t.primary };
    if kx > a {
        c.rounded_rect(a, cy - track_h / 2.0, (kx - a).max(track_h), track_h, track_h / 2.0, fill);
    }
    c.circle(kx, cy, if drag { 9.0 } else { 7.0 }, fill);
    let pct = format!("{}%", (ch.volume * 100.0).round() as i32);
    let pct_col = if drag { t.primary } else { t.on_surface_variant };
    fonts.draw_text_right(c, &pct, x + w - 10.0, cy + 4.5, 13.0, 40.0, pct_col);
}

/// Righe visibili, indicatore di scorrimento e messaggio se non c'è audio.
pub fn draw_rows(c: &mut Canvas, fonts: &Fonts, t: &Theme, l: &Layout, v: &View) {
    let area = l.rows_area();
    c.fill_rect(area.x, area.y, area.w, area.h, t.background);
    for slot in 0..l.visible {
        draw_row(c, fonts, t, l, v, slot);
    }
    let n = v.channels.len();
    if n == 0 {
        let cy = (area.y + area.h / 2) as f32;
        fonts.draw_icon(c, "volume_mute", false, l.w as f32 / 2.0, cy - 14.0, 28.0, t.outline);
        fonts.draw_text(c, tr!("Nessun audio"), l.w as f32 / 2.0, cy + 18.0, 13.0, l.w as f32 - 32.0, t.on_surface_variant);
    } else if n > l.visible && l.vertical {
        // Barretta nel margine in basso: posizione e porzione visibile dell'elenco.
        let (left, total) = (M as f32, (l.w - 2 * M) as f32);
        let len = (total * l.visible as f32 / n as f32).max(12.0);
        let pos = left + (total - len) * v.scroll as f32 / l.max_scroll(n) as f32;
        let y = (l.h - M + 3) as f32;
        c.rounded_rect(left, y, total, 3.0, 1.5, t.surface_container);
        c.rounded_rect(pos, y, len, 3.0, 1.5, t.outline);
    } else if n > l.visible {
        // Barretta nel margine destro: posizione e porzione visibile dell'elenco.
        let top = l.row(0).y as f32;
        let total = (l.row(l.visible - 1).y + ROW_H) as f32 - top;
        let len = (total * l.visible as f32 / n as f32).max(12.0);
        let pos = top + (total - len) * v.scroll as f32 / l.max_scroll(n) as f32;
        c.rounded_rect((l.w - M + 2) as f32, top, 3.0, total, 1.5, t.surface_container);
        c.rounded_rect((l.w - M + 2) as f32, pos, 3.0, len, 1.5, t.outline);
    }
}

pub fn draw_all(c: &mut Canvas, fonts: &Fonts, t: &Theme, l: &Layout, v: &View) {
    c.fill(t.background);
    draw_card(c, fonts, t, l, v);
    draw_rows(c, fonts, t, l, v);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_and_hits() {
        let l = Layout::new(&MixerConfig { vertical: false, ..MixerConfig::default() }, 320, 240, 0);
        assert_eq!((l.rows_y, l.visible), (88, 4));
        let full = Layout::new(&MixerConfig { player: false, vertical: false, ..MixerConfig::default() }, 320, 240, 0);
        assert_eq!((full.rows_y, full.visible, full.card), (8, 6, None));
        assert!(l.row(3).y + ROW_H <= 240 - M);

        let (px, py, _) = l.button(Button::PlayPause).unwrap();
        assert_eq!(l.hit(px as i32, py as i32, 0, 3), Some(Hit::Button(Button::PlayPause)));
        assert_eq!(l.hit(40, 40, 0, 3), Some(Hit::Card));
        let r = l.row(1);
        assert_eq!(l.hit(20, (r.y + 10) as i32, 0, 3), Some(Hit::Mute(1)));
        assert_eq!(l.hit(200, (r.y + 10) as i32, 1, 3), Some(Hit::Slider(2)));
        assert_eq!(l.hit(200, (r.y + 10) as i32, 2, 3), None);
        let (a, b) = l.bar();
        assert_eq!((l.value_at(a as i32 - 20, 0), l.value_at(b as i32 + 5, 0)), (0.0, 1.0));
        assert_eq!(l.value_at(((a + b) / 2.0) as i32, 0), 0.5);
    }

    #[test]
    fn vertical_layout() {
        let l = Layout::new(&MixerConfig::default(), 320, 240, 0);
        assert!(l.vertical);
        assert_eq!(l.visible, 5);
        // 5 colonne riempiono esattamente la larghezza tra i margini.
        let (first, last) = (l.cell(0, 9), l.cell(4, 9));
        assert_eq!((first.x, last.x + last.w), (M, 320 - M));
        // Con 2 canali le colonne sono centrate.
        let (a, b) = (l.cell(0, 2), l.cell(1, 2));
        assert!((a.x as i32 - (320 - (b.x + b.w)) as i32).abs() <= 1);
        let (_, top, _, h) = l.pill(a);
        let cx = (a.x + a.w / 2) as i32;
        assert_eq!(l.hit(cx, (top + 10.0) as i32, 0, 2), Some(Hit::Slider(0)));
        assert_eq!(l.hit(cx, (a.y + a.h - 5) as i32, 0, 2), Some(Hit::Mute(0)));
        let c5 = l.cell(1, 5);
        assert_eq!(l.hit((c5.x + 5) as i32, (top + 50.0) as i32, 3, 5), Some(Hit::Slider(4)));
        assert_eq!((l.value_at(0, top as i32), l.value_at(0, (top + h) as i32)), (1.0, 0.0));
    }
}
