use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::app::{Event, Phase};
use crate::config::{self, Touch};
use crate::device::{self, Device};
use crate::render::{Canvas, Fonts};
use crate::theme;

/// Chiede tre tocchi su mirini noti e ricava l'orientamento del touch.
pub fn run(cfg_path: &Path) -> Result<()> {
    let cfg = config::load(cfg_path)?;
    let theme = theme::for_config(&cfg.theme, &config::scheme_path());
    let fonts = Fonts::load(&cfg.layout.icon_font, &cfg.layout.text_font)?;
    let path = device::find_port(&cfg.device.port).context(tr!("scheda non trovata (prova `cyd-deck ports`)"))?;
    let (tx, rx) = mpsc::channel();
    let mut dev = Device::open(&path, tx)?;
    dev.handshake(&rx)?;
    dev.madctl(device::madctl(cfg.device.rotation, cfg.device.bgr))?;
    dev.invert(cfg.device.invert)?;
    dev.backlight(true)?;

    let (w, h) = device::screen_size(cfg.device.rotation);
    let m = 28;
    let targets = [(m, m), (w - m, m), (m, h - m)];
    let mut canvas = Canvas::new(w, h);
    let mut raw = Vec::new();
    for (n, &(tx_, ty)) in targets.iter().enumerate() {
        canvas.fill(theme.background);
        let msg = tr!("Tocca il centro del mirino ({}/3)", n + 1);
        fonts.draw_text(&mut canvas, &msg, w as f32 / 2.0, h as f32 / 2.0, 16.0, w as f32 - 20.0, theme.on_surface);
        canvas.ring(tx_ as f32, ty as f32, 14.0, 2.0, theme.primary);
        canvas.circle(tx_ as f32, ty as f32, 3.0, theme.primary);
        dev.blit(0, 0, w, h, &canvas.rgb565(0, 0, w, h))?;
        println!("{}", tr!("tocca il mirino {}/3 sullo schermo...", n + 1));
        loop {
            match rx.recv()? {
                Event::Touch(Phase::Up, x, y) => {
                    raw.push((x as i32, y as i32));
                    break;
                }
                Event::Disconnected => anyhow::bail!("{}", tr!("scheda scollegata")),
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(250));
        while rx.try_recv().is_ok() {}
    }

    // Da sinistra a destra cambia solo la X dello schermo, dall'alto in basso solo la Y.
    let right = (raw[1].0 - raw[0].0, raw[1].1 - raw[0].1);
    let down = (raw[2].0 - raw[0].0, raw[2].1 - raw[0].1);
    let swap_xy = right.1.abs() > right.0.abs();
    let t = Touch {
        swap_xy,
        invert_x: if swap_xy { right.1 < 0 } else { right.0 < 0 },
        invert_y: if swap_xy { down.0 < 0 } else { down.1 < 0 },
    };
    println!("coordinate grezze: {raw:?}");
    println!("[touch]\nswap_xy = {}\ninvert_x = {}\ninvert_y = {}", t.swap_xy, t.invert_x, t.invert_y);
    if config::write_touch(cfg_path, &t)? {
        println!("{}", tr!("salvato in {}", cfg_path.display()));
    } else {
        println!("{}", tr!("sezione [touch] non trovata: copia i valori qui sopra in {}", cfg_path.display()));
    }

    canvas.fill(theme.background);
    fonts.draw_text(&mut canvas, "Calibrazione completata", w as f32 / 2.0, h as f32 / 2.0, 18.0, w as f32 - 20.0, theme.primary);
    dev.blit(0, 0, w, h, &canvas.rgb565(0, 0, w, h))?;
    Ok(())
}
