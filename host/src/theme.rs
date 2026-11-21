use std::path::Path;

use serde_json::Value;

use crate::render::Rgb;

/// Colori Material You presi dallo schema corrente di Caelestia.
#[derive(Debug, Clone)]
pub struct Theme {
    pub background: Rgb,
    pub surface_container: Rgb,
    pub primary: Rgb,
    pub on_primary: Rgb,
    pub on_surface: Rgb,
    pub on_surface_variant: Rgb,
    pub secondary_container: Rgb,
    pub on_secondary_container: Rgb,
    pub outline_variant: Rgb,
    pub outline: Rgb,
    pub surface_container_low: Rgb,
    pub surface_container_high: Rgb,
    pub surface_container_highest: Rgb,
    pub error: Rgb,
    pub dark: bool,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            background: hex("18120e").unwrap(),
            surface_container: hex("241e1a").unwrap(),
            primary: hex("ffb878").unwrap(),
            on_primary: hex("4c2700").unwrap(),
            on_surface: hex("ece0d9").unwrap(),
            on_surface_variant: hex("d6c3b5").unwrap(),
            secondary_container: hex("5c412a").unwrap(),
            on_secondary_container: hex("d3ae90").unwrap(),
            outline_variant: hex("52443a").unwrap(),
            outline: hex("9f8e81").unwrap(),
            surface_container_low: hex("201a16").unwrap(),
            surface_container_high: hex("2f2924").unwrap(),
            surface_container_highest: hex("3a332e").unwrap(),
            error: hex("ffb4ab").unwrap(),
            dark: true,
        }
    }
}

fn hex(s: &str) -> Option<Rgb> {
    let s = s.trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some([(v >> 16) as f32 / 255.0, ((v >> 8) & 0xff) as f32 / 255.0, (v & 0xff) as f32 / 255.0])
}

/// Carica scheme.json; ogni colore mancante resta quello di default.
pub fn load(path: &Path) -> Theme {
    let mut t = Theme::default();
    let scheme = std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok());
    if let Some(mode) = scheme.as_ref().and_then(|v| v.get("mode")).and_then(Value::as_str) {
        t.dark = mode != "light";
    }
    let Some(colours) = scheme.and_then(|v| v.get("colours").cloned()) else {
        eprintln!("{}", tr!("schema Caelestia non leggibile ({}), uso i colori di default", path.display()));
        return t;
    };
    let get = |name: &str, slot: &mut Rgb| {
        if let Some(c) = colours.get(name).and_then(Value::as_str).and_then(hex) {
            *slot = c;
        }
    };
    get("background", &mut t.background);
    get("surfaceContainer", &mut t.surface_container);
    get("primary", &mut t.primary);
    get("onPrimary", &mut t.on_primary);
    get("onSurface", &mut t.on_surface);
    get("onSurfaceVariant", &mut t.on_surface_variant);
    get("secondaryContainer", &mut t.secondary_container);
    get("onSecondaryContainer", &mut t.on_secondary_container);
    get("outlineVariant", &mut t.outline_variant);
    get("outline", &mut t.outline);
    get("surfaceContainerLow", &mut t.surface_container_low);
    get("surfaceContainerHigh", &mut t.surface_container_high);
    get("surfaceContainerHighest", &mut t.surface_container_highest);
    get("error", &mut t.error);
    t
}

/// Temi pronti: (nome in italiano, colore principale per il tema scuro).
pub const PRESETS: &[(&str, &str)] = &[
    ("Ambra", "#ffb878"),
    ("Oceano", "#7cc4ff"),
    ("Foresta", "#8fd694"),
    ("Lavanda", "#c5b3ff"),
    ("Ciliegia", "#ff8fa3"),
    ("Limone", "#e6d36a"),
    ("Menta", "#6fdccb"),
    ("Grafite", "#c4c7cc"),
];

fn to_hsl([r, g, b]: Rgb) -> (f32, f32, f32) {
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.0;
    let d = max - min;
    if d < 1e-5 {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    } * 60.0;
    (h, s.clamp(0.0, 1.0), l)
}

fn from_hsl(h: f32, s: f32, l: f32) -> Rgb {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h.rem_euclid(360.0) / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r + m, g + m, b + m]
}

fn lum([r, g, b]: Rgb) -> f32 {
    let f = |c: f32| if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
    0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b)
}

/// Rapporto di contrasto WCAG tra due colori (1..21).
fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (x, y) = (lum(a), lum(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// Luminosità più vicina a `l` (spostandosi verso `step`) con cui `h,s` contrasta con `other`.
fn readable(h: f32, s: f32, mut l: f32, step: f32, other: Rgb, min: f32) -> Rgb {
    while contrast(from_hsl(h, s, l), other) < min && (0.02..=0.98).contains(&l) {
        l += step;
    }
    from_hsl(h, s, l)
}

/// Tema completo ricavato da un solo colore: superfici tinte appena della sua tonalità,
/// colore principale e contrasti alla Material You.
pub fn from_accent(accent: Rgb, light: bool) -> Theme {
    let (h, s, _) = to_hsl(accent);
    // Grigi tinti: poca saturazione, così il colore resta un accento.
    let tint = (s * 0.35).min(0.22);
    let c = |sat: f32, l: f32| from_hsl(h, sat, l);
    if light {
        Theme {
            background: c(tint, 0.975),
            surface_container_low: c(tint, 0.955),
            surface_container: c(tint, 0.935),
            surface_container_high: c(tint, 0.91),
            surface_container_highest: c(tint, 0.885),
            primary: readable(h, s.min(0.75), 0.42, -0.01, [1.0; 3], 4.8),
            on_primary: [1.0; 3],
            on_surface: c(tint, 0.11),
            on_surface_variant: c(tint, 0.30),
            secondary_container: c((s * 0.6).min(0.5), 0.86),
            on_secondary_container: c((s * 0.6).min(0.5), 0.18),
            outline_variant: c(tint, 0.80),
            outline: c(tint, 0.47),
            error: hex("ba1a1a").unwrap(),
            dark: false,
        }
    } else {
        Theme {
            background: c(tint, 0.065),
            surface_container_low: c(tint, 0.095),
            surface_container: c(tint, 0.115),
            surface_container_high: c(tint, 0.15),
            surface_container_highest: c(tint, 0.19),
            primary: readable(h, s, to_hsl(accent).2.clamp(0.62, 0.85), 0.01, c(s.min(0.8), 0.15), 4.8),
            on_primary: c(s.min(0.8), 0.15),
            on_surface: c(tint, 0.90),
            on_surface_variant: c(tint, 0.78),
            secondary_container: c((s * 0.45).min(0.4), 0.24),
            on_secondary_container: c((s * 0.6).min(0.5), 0.84),
            outline_variant: c(tint, 0.28),
            outline: c(tint, 0.58),
            error: hex("ffb4ab").unwrap(),
            dark: true,
        }
    }
}

/// Colore "#rrggbb" della config, se valido.
pub fn parse_hex(s: &str) -> Option<Rgb> {
    hex(s.trim())
}

/// Tema scelto nella config: Caelestia (se c'è e se richiesto) o quello del colore principale.
pub fn for_config(cfg: &crate::config::ThemeCfg, scheme: &Path) -> Theme {
    let caelestia = match cfg.style.as_str() {
        "custom" => false,
        "caelestia" => true,
        _ => scheme.exists(),
    };
    if caelestia {
        return load(scheme);
    }
    from_accent(parse_hex(&cfg.accent).unwrap_or(Theme::default().primary), cfg.light)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ogni tema pronto, chiaro e scuro, deve restare leggibile.
    #[test]
    fn presets_are_readable() {
        for (name, accent) in PRESETS {
            for light in [false, true] {
                let t = from_accent(parse_hex(accent).unwrap(), light);
                assert!(contrast(t.on_surface, t.surface_container) >= 7.0, "{name} {light}: testo");
                assert!(contrast(t.on_surface_variant, t.surface_container) >= 4.5, "{name} {light}: testo secondario");
                assert!(contrast(t.on_primary, t.primary) >= 4.5, "{name} {light}: testo sul colore");
                assert!(contrast(t.on_secondary_container, t.secondary_container) >= 4.5, "{name} {light}: tasti attivi");
                assert!(contrast(t.primary, t.background) >= 3.0, "{name} {light}: accento");
            }
        }
    }

    #[test]
    fn hsl_round_trip() {
        for c in [[1.0, 0.72, 0.47], [0.2, 0.5, 0.9], [0.5, 0.5, 0.5]] {
            let (h, s, l) = to_hsl(c);
            let back = from_hsl(h, s, l);
            assert!(c.iter().zip(back).all(|(a, b)| (a - b).abs() < 1e-3), "{c:?} → {back:?}");
        }
    }
}
