#[macro_use]
mod i18n;
mod app;
mod audio;
mod bar;
mod cached;
mod calibrate;
mod claude;
mod config;
mod device;
mod gui;
mod icons;
mod macros;
mod media;
mod mixer;
mod presets;
mod printer;
mod render;
mod secrets;
mod ha;
mod theme;
mod widgets;

use std::process::ExitCode;

const USAGE: &str = "\
cyd-deck - macropad stile Stream Deck per Freenove ESP32-S3 2.8\"

uso:
  cyd-deck [run]      avvia il demone (default)
  cyd-deck gui        apre l'editor grafico dei tasti
  cyd-deck calibrate  calibra l'orientamento del touch
  cyd-deck ports      elenca le porte seriali
  cyd-deck preview [dir]  salva le pagine come immagini PPM
  cyd-deck config     stampa il percorso della config
  cyd-deck ha ...     comandi per Home Assistant (cyd-deck ha per l'aiuto)
  cyd-deck printer ...  stampante Bambu Lab (cyd-deck printer per l'aiuto)
  cyd-deck cached <min> <cmd>  ultimo risultato del comando, rinnovato ogni <min> minuti
  cyd-deck statusline status line per Claude Code: salva i limiti d'uso per il deck";

fn main() -> ExitCode {
    // I comandi dei tasti possono richiamare `cyd-deck ...` anche se ~/.cargo/bin non è nel PATH.
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.to_path_buf())) {
        let path = std::env::var_os("PATH").unwrap_or_default();
        if !std::env::split_paths(&path).any(|p| p == dir) {
            let joined = std::env::join_paths(std::iter::once(dir).chain(std::env::split_paths(&path))).unwrap_or(path);
            // SAFETY: all'avvio, prima di creare thread.
            unsafe { std::env::set_var("PATH", joined) };
        }
    }
    // Interrogato dai tasti ogni pochi secondi: niente config, risposta immediata.
    if std::env::args().nth(1).as_deref() == Some("cached") {
        return cached::run(&std::env::args().skip(2).collect::<Vec<_>>());
    }
    // Chiamato da Claude Code a ogni messaggio: deve essere rapido e non toccare la config.
    if std::env::args().nth(1).as_deref() == Some("statusline") {
        return match claude::statusline() {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        };
    }
    let cfg_path = config::config_path();
    // Lingua del sistema per la prima config; poi vale quella scelta nella config.
    i18n::set("auto");
    if let Err(e) = config::ensure_exists(&cfg_path) {
        eprintln!("{}", tr!("impossibile creare la config: {}", format!("{e:#}")));
        return ExitCode::FAILURE;
    }
    i18n::set(&config::load(&cfg_path).map(|c| c.language).unwrap_or_default());
    let result = match std::env::args().nth(1).as_deref() {
        None | Some("run") => app::run(&cfg_path),
        Some("gui") => gui::run(&cfg_path),
        Some("calibrate") => calibrate::run(&cfg_path),
        Some("ha") => ha::cli(&std::env::args().skip(2).collect::<Vec<_>>()),
        Some("printer") => printer::cli(&std::env::args().skip(2).collect::<Vec<_>>()),
        Some("ports") => {
            device::list_ports();
            Ok(())
        }
        Some("preview") => {
            let dir = std::env::args().nth(2).unwrap_or_else(|| "cyd-deck-preview".into());
            app::preview(&cfg_path, std::path::Path::new(&dir))
        }
        Some("config") => {
            println!("{}", cfg_path.display());
            Ok(())
        }
        _ => {
            println!("{}", i18n::t(USAGE));
            Ok(())
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{}", tr!("errore: {}", format!("{e:#}")));
            ExitCode::FAILURE
        }
    }
}
