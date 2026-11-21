//! Barra di stato in cima al deck: nome della pagina a sinistra, ora, collegamento e batteria a destra.

use crate::config::Bar;
use crate::device::Link;
use crate::render::{Canvas, Fonts};
use crate::theme::Theme;

pub struct Info<'a> {
    pub page: &'a str,
    pub clock: &'a str,
    /// None = scheda senza batteria (o firmware che non la riporta).
    pub battery: Option<u8>,
    pub charging: bool,
    pub link: Option<Link>,
}

const SIZE: f32 = 12.0;
const BASELINE: f32 = 15.0;
const CY: f32 = 11.0;

/// Data breve nella lingua del sistema, es. "dom 25 ott".
pub fn date() -> String {
    crate::app::shell_output("date '+%a %-d %b'")
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Ora locale "HH:MM".
pub fn clock() -> String {
    crate::app::shell_output("date +%H:%M")
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

pub fn draw(c: &mut Canvas, fonts: &Fonts, t: &Theme, bar: &Bar, height: u32, info: &Info) {
    if height == 0 {
        return;
    }
    let w = c.w;
    c.fill_rect(0, 0, w, height, t.background);
    let fg = t.on_surface_variant;
    let mut x = w as f32 - 10.0;

    if bar.battery {
        if let Some(p) = info.battery {
            let col = if p <= 15 && !info.charging { t.error } else { fg };
            let text = format!("{p}%");
            fonts.draw_text_right(c, &text, x, BASELINE, SIZE, 40.0, col);
            x -= fonts.text_width(&text, SIZE) + 10.0;
            let icon = crate::app::battery_icon(Some(p), info.charging);
            fonts.draw_icon(c, icon, true, x, CY, 16.0, col);
            x -= 18.0;
        }
    }
    if bar.link {
        if let Some(link) = info.link {
            let icon = match link {
                Link::Usb => "usb",
                Link::Wifi => "wifi",
            };
            fonts.draw_icon(c, icon, true, x - 2.0, CY, 15.0, fg);
            x -= 22.0;
        }
    }
    if bar.clock && !info.clock.is_empty() {
        fonts.draw_text_right(c, info.clock, x, BASELINE, SIZE, 60.0, t.on_surface);
        x -= fonts.text_width(info.clock, SIZE) + 10.0;
    }
    if bar.page {
        let max_w = (x - 10.0 - 10.0).max(0.0);
        fonts.draw_text_left(c, info.page, 10.0, BASELINE, SIZE, max_w, t.primary);
    }
}
