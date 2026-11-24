use std::process::Command;

use ab_glyph::{Font, FontRef, GlyphId, PxScale, ScaleFont, VariableFont, point};
use anyhow::{Context, Result, bail};

use crate::theme::Theme;

pub type Rgb = [f32; 3];

/// Framebuffer RGB in memoria, convertito in RGB565 solo al momento del blit.
pub struct Canvas {
    pub w: u32,
    pub h: u32,
    px: Vec<Rgb>,
}

impl Canvas {
    pub fn new(w: u32, h: u32) -> Self {
        Self { w, h, px: vec![[0.0; 3]; (w * h) as usize] }
    }

    pub fn fill(&mut self, c: Rgb) {
        self.px.fill(c);
    }

    pub fn fill_rect(&mut self, x: u32, y: u32, w: u32, h: u32, c: Rgb) {
        for yy in y..(y + h).min(self.h) {
            for xx in x..(x + w).min(self.w) {
                self.px[(yy * self.w + xx) as usize] = c;
            }
        }
    }

    fn blend(&mut self, x: i32, y: i32, c: Rgb, a: f32) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 || a <= 0.0 {
            return;
        }
        let a = a.min(1.0);
        let p = &mut self.px[(y as u32 * self.w + x as u32) as usize];
        for k in 0..3 {
            p[k] = p[k] * (1.0 - a) + c[k] * a;
        }
    }

    /// Rettangolo arrotondato con antialiasing (distanza con segno).
    pub fn rounded_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, c: Rgb) {
        let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        let (hx, hy) = (w / 2.0 - r, h / 2.0 - r);
        for py in y.floor() as i32..(y + h).ceil() as i32 {
            for px in x.floor() as i32..(x + w).ceil() as i32 {
                let qx = (px as f32 + 0.5 - cx).abs() - hx;
                let qy = (py as f32 + 0.5 - cy).abs() - hy;
                let d = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r;
                self.blend(px, py, c, 0.5 - d);
            }
        }
    }

    pub fn circle(&mut self, cx: f32, cy: f32, r: f32, c: Rgb) {
        self.ring(cx, cy, r, f32::INFINITY, c);
    }

    /// Anello di raggio `r` e spessore `width` (INFINITY = disco pieno).
    pub fn ring(&mut self, cx: f32, cy: f32, r: f32, width: f32, c: Rgb) {
        for py in (cy - r - 1.0).floor() as i32..=(cy + r + 1.0).ceil() as i32 {
            for px in (cx - r - 1.0).floor() as i32..=(cx + r + 1.0).ceil() as i32 {
                let d = (px as f32 + 0.5 - cx).hypot(py as f32 + 0.5 - cy);
                let outer = (r - d + 0.5).clamp(0.0, 1.0);
                let inner = (d - (r - width) + 0.5).clamp(0.0, 1.0);
                self.blend(px, py, c, outer.min(inner));
            }
        }
    }

    /// Immagine `size`×`size` con angoli arrotondati di raggio `r`.
    pub fn image_rounded(&mut self, x: f32, y: f32, size: u32, r: f32, img: &[Rgb]) {
        let s = size as f32;
        let (c, h) = (s / 2.0, s / 2.0 - r);
        for iy in 0..size {
            for ix in 0..size {
                let qx = (ix as f32 + 0.5 - c).abs() - h;
                let qy = (iy as f32 + 0.5 - c).abs() - h;
                let d = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r;
                if let Some(&p) = img.get((iy * size + ix) as usize) {
                    self.blend(x as i32 + ix as i32, y as i32 + iy as i32, p, 0.5 - d);
                }
            }
        }
    }

    /// Tutto il canvas come RGBA8, per le anteprime nella GUI.
    pub fn rgba(&self) -> Vec<u8> {
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        self.px.iter().flat_map(|&[r, g, b]| [q(r), q(g), q(b), 255]).collect()
    }

    /// Estrae un rettangolo come RGB565 big-endian, il formato che l'ILI9341 si aspetta.
    pub fn rgb565(&self, x: u32, y: u32, w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity((w * h * 2) as usize);
        for yy in y..y + h {
            for xx in x..x + w {
                let [r, g, b] = self.px[(yy * self.w + xx) as usize];
                let q = |v: f32, max: f32| (v.clamp(0.0, 1.0) * max + 0.5) as u16;
                let v = (q(r, 31.0) << 11) | (q(g, 63.0) << 5) | q(b, 31.0);
                out.extend_from_slice(&v.to_be_bytes());
            }
        }
        out
    }
}

pub struct Fonts {
    pub icon_data: &'static [u8],
    pub text_data: &'static [u8],
    icon: FontRef<'static>,
    icon_fill: FontRef<'static>,
    icon_face: rustybuzz::Face<'static>,
    text: FontRef<'static>,
}

/// Font inclusi nel programma (licenze in fonts/): funziona senza installare niente.
const BUNDLED_ICONS: &[u8] = include_bytes!("../fonts/MaterialSymbolsRounded.ttf");
const BUNDLED_TEXT: &[u8] = include_bytes!("../fonts/Rubik.ttf");

fn find_font(family: &str) -> Result<&'static [u8]> {
    match family {
        "" | "Material Symbols Rounded" => return Ok(BUNDLED_ICONS),
        "Rubik" => return Ok(BUNDLED_TEXT),
        _ => {}
    }
    // Un font scelto nella config: lo si chiede al sistema.
    let out = Command::new("fc-match").args(["-f", "%{file}", family]).output().context("fc-match")?;
    let path = String::from_utf8_lossy(&out.stdout).to_string();
    if path.is_empty() {
        bail!("font '{family}' non trovato");
    }
    let data = std::fs::read(&path).with_context(|| format!("lettura font {path}"))?;
    // I font restano in memoria per tutta la vita del processo.
    Ok(Box::leak(data.into_boxed_slice()))
}

impl Fonts {
    pub fn load(icon_family: &str, text_family: &str) -> Result<Self> {
        let icon_data = find_font(icon_family)?;
        let mut icon = FontRef::try_from_slice(icon_data)?;
        let mut icon_fill = FontRef::try_from_slice(icon_data)?;
        for f in [&mut icon, &mut icon_fill] {
            f.set_variation(b"wght", 400.0);
            f.set_variation(b"opsz", 40.0);
        }
        icon_fill.set_variation(b"FILL", 1.0);
        let icon_face = rustybuzz::Face::from_slice(icon_data, 0).context("font icone non valido")?;

        let text_data = find_font(text_family)?;
        let mut text = FontRef::try_from_slice(text_data)?;
        text.set_variation(b"wght", 500.0);
        Ok(Self { icon_data, text_data, icon, icon_fill, icon_face, text })
    }

    /// Risolve il nome di un'icona tramite le ligature del font (es. "volume_up" -> glifo).
    pub fn icon_glyph(&self, name: &str) -> Option<GlyphId> {
        let mut buf = rustybuzz::UnicodeBuffer::new();
        buf.push_str(name);
        let shaped = rustybuzz::shape(&self.icon_face, &[], buf);
        match shaped.glyph_infos() {
            [g] if g.glyph_id != 0 => Some(GlyphId(g.glyph_id as u16)),
            _ => None,
        }
    }

    fn layout(&self, text: &str, size: f32) -> (Vec<(GlyphId, f32)>, f32) {
        let sf = self.text.as_scaled(PxScale::from(size));
        let mut x = 0.0;
        let mut prev = None;
        let mut glyphs = Vec::new();
        for ch in text.chars() {
            let id = sf.glyph_id(ch);
            if let Some(p) = prev {
                x += sf.kern(p, id);
            }
            glyphs.push((id, x));
            x += sf.h_advance(id);
            prev = Some(id);
        }
        (glyphs, x)
    }

    /// Riduce la dimensione e, se serve, tronca con "…" finché il testo sta in `max_w`.
    fn fit(&self, text: &str, size: f32, max_w: f32) -> (String, f32) {
        let min = size * 0.8;
        let mut s = size;
        while s > min && self.layout(text, s).1 > max_w {
            s -= 0.5;
        }
        if self.layout(text, s).1 <= max_w {
            return (text.to_string(), s);
        }
        let mut chars: Vec<char> = text.chars().collect();
        while !chars.is_empty() {
            chars.pop();
            let t: String = chars.iter().collect::<String>().trim_end().to_string() + "…";
            if self.layout(&t, s).1 <= max_w {
                return (t, s);
            }
        }
        (String::new(), s)
    }

    /// Larghezza del testo come verrebbe disegnato (senza riduzioni).
    pub fn text_width(&self, text: &str, size: f32) -> f32 {
        self.layout(text, size).1
    }

    /// Disegna testo centrato orizzontalmente su `cx` con la linea di base a `baseline`.
    pub fn draw_text(&self, c: &mut Canvas, text: &str, cx: f32, baseline: f32, size: f32, max_w: f32, color: Rgb) {
        self.draw_text_at(c, text, cx, 0.5, baseline, size, max_w, color);
    }

    /// Come `draw_text`, ma il testo parte da `x`.
    pub fn draw_text_left(&self, c: &mut Canvas, text: &str, x: f32, baseline: f32, size: f32, max_w: f32, color: Rgb) {
        self.draw_text_at(c, text, x, 0.0, baseline, size, max_w, color);
    }

    /// Come `draw_text`, ma il testo finisce a `x`.
    pub fn draw_text_right(&self, c: &mut Canvas, text: &str, x: f32, baseline: f32, size: f32, max_w: f32, color: Rgb) {
        self.draw_text_at(c, text, x, 1.0, baseline, size, max_w, color);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_text_at(&self, c: &mut Canvas, text: &str, x: f32, align: f32, baseline: f32, size: f32, max_w: f32, color: Rgb) {
        let (text, size) = self.fit(text, size, max_w);
        let (glyphs, width) = self.layout(&text, size);
        let x0 = (x - width * align).round();
        for (id, gx) in glyphs {
            let g = id.with_scale_and_position(PxScale::from(size), point(x0 + gx, baseline.round()));
            if let Some(og) = self.text.outline_glyph(g) {
                let b = og.px_bounds();
                og.draw(|x, y, a| c.blend(b.min.x as i32 + x as i32, b.min.y as i32 + y as i32, color, a.powf(0.85)));
            }
        }
    }

    /// Icona come immagine RGBA bianca con alfa = copertura, da colorare con una tinta.
    pub fn icon_mask(&self, name: &str, px: u32) -> Option<Vec<u8>> {
        let id = self.icon_glyph(name)?;
        let og = self.icon.outline_glyph(id.with_scale_and_position(PxScale::from(px as f32 * 0.9), point(0.0, 0.0)))?;
        let b = og.px_bounds();
        let dx = (px as f32 / 2.0 - (b.min.x + b.max.x) / 2.0).round() as i32;
        let dy = (px as f32 / 2.0 - (b.min.y + b.max.y) / 2.0).round() as i32;
        let mut out = vec![0u8; (px * px * 4) as usize];
        og.draw(|x, y, a| {
            let (x, y) = (b.min.x as i32 + x as i32 + dx, b.min.y as i32 + y as i32 + dy);
            if x >= 0 && y >= 0 && (x as u32) < px && (y as u32) < px {
                let i = ((y as u32 * px + x as u32) * 4) as usize;
                out[i..i + 4].copy_from_slice(&[255, 255, 255, (a.min(1.0) * 255.0) as u8]);
            }
        });
        Some(out)
    }

    /// Se l'icona esiste nel font.
    pub fn has_icon(&self, name: &str) -> bool {
        self.icon_glyph(name).is_some()
    }

    /// Disegna un'icona centrata in (cx, cy). Restituisce false se il nome non esiste nel font.
    pub fn draw_icon(&self, c: &mut Canvas, name: &str, filled: bool, cx: f32, cy: f32, size: f32, color: Rgb) -> bool {
        let Some(id) = self.icon_glyph(name) else { return false };
        let font = if filled { &self.icon_fill } else { &self.icon };
        let Some(og) = font.outline_glyph(id.with_scale_and_position(PxScale::from(size), point(0.0, 0.0))) else {
            return false;
        };
        let b = og.px_bounds();
        let dx = (cx - (b.min.x + b.max.x) / 2.0).round() as i32;
        let dy = (cy - (b.min.y + b.max.y) / 2.0).round() as i32;
        og.draw(|x, y, a| c.blend(b.min.x as i32 + x as i32 + dx, b.min.y as i32 + y as i32 + dy, color, a));
        true
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Normal,
    Active,
    Pressed,
}

pub struct KeyVisual<'a> {
    pub icon: Option<&'a str>,
    pub label: Option<&'a str>,
    pub style: Style,
}

/// Disegna un tasto quadrato di lato `s` con angolo in (x, y).
pub fn draw_key(c: &mut Canvas, fonts: &Fonts, theme: &Theme, x: u32, y: u32, s: u32, radius: f32, k: &KeyVisual) {
    c.fill_rect(x, y, s, s, theme.background);
    let (tile, icon_col, label_col, inset) = match k.style {
        Style::Normal => (theme.surface_container, theme.primary, theme.on_surface_variant, 0.0),
        Style::Active => (theme.primary, theme.on_primary, theme.on_primary, 0.0),
        Style::Pressed => (theme.secondary_container, theme.on_secondary_container, theme.on_secondary_container, 3.0),
    };
    let (fx, fy, fs) = (x as f32, y as f32, s as f32);
    c.rounded_rect(fx + inset, fy + inset, fs - 2.0 * inset, fs - 2.0 * inset, radius - inset, tile);

    let filled = k.style != Style::Normal;
    let cx = fx + fs / 2.0;
    let max_w = fs - 10.0;
    match (k.icon, k.label) {
        (Some(icon), Some(label)) => {
            if !fonts.draw_icon(c, icon, filled, cx, fy + fs * 0.40, fs * 0.44, icon_col) {
                fonts.draw_text(c, icon, cx, fy + fs * 0.48, fs * 0.2, max_w, icon_col);
            }
            fonts.draw_text(c, label, cx, fy + fs * 0.85, fs * 0.17, max_w, label_col);
        }
        (Some(icon), None) => {
            if !fonts.draw_icon(c, icon, filled, cx, fy + fs / 2.0, fs * 0.56, icon_col) {
                fonts.draw_text(c, icon, cx, fy + fs * 0.58, fs * 0.2, max_w, icon_col);
            }
        }
        (None, Some(label)) => {
            let size = fs * 0.24;
            fonts.draw_text(c, label, cx, fy + fs / 2.0 + size * 0.35, size, max_w, icon_col);
        }
        (None, None) => {}
    }
}
