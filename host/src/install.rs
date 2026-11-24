//! Integrazione con il desktop: voce nel menu delle app, icona, avvio all'accesso e avvio del
//! programma in background. Funziona sia da `cargo install` sia dall'AppImage.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

const ICON: &str = include_str!("../assets/cyd-deck.svg");

/// Il file da lanciare: dentro un'AppImage è il file .AppImage, non il programma estratto
/// in una cartella temporanea che sparisce alla chiusura.
pub fn exe() -> PathBuf {
    std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| PathBuf::from("cyd-deck"))
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".local/share"))
}

fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config"))
}

fn menu_entry() -> PathBuf {
    data_dir().join("applications/cyd-deck.desktop")
}

fn icon_file() -> PathBuf {
    data_dir().join("icons/hicolor/scalable/apps/cyd-deck.svg")
}

fn autostart_entry() -> PathBuf {
    config_dir().join("autostart/cyd-deck.desktop")
}

/// Il percorso tra virgolette, come vuole la specifica dei file .desktop.
fn quoted_exe() -> String {
    let p = exe().display().to_string();
    format!("\"{}\"", p.replace('\\', "\\\\").replace('"', "\\\"").replace('`', "\\`").replace('$', "\\$"))
}

fn write(path: &PathBuf, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, text).with_context(|| format!("{}", path.display()))
}

/// Mette l'editor nel menu delle app (con l'icona).
pub fn add_to_menu() -> Result<()> {
    write(&icon_file(), ICON)?;
    write(
        &menu_entry(),
        &format!(
            "[Desktop Entry]\nType=Application\nName=CYD Deck\nGenericName=Macropad editor\nGenericName[it]=Editor macropad\n\
             Comment=Edit the keys of your CYD Deck macropad\nComment[it]=Modifica i tasti del macropad CYD Deck\n\
             Exec={} gui\nIcon=cyd-deck\nCategories=Utility;\nKeywords=stream;deck;macropad;cyd;esp32;\nStartupWMClass=cyd-deck\n",
            quoted_exe()
        ),
    )
}

/// Fa partire il programma in background all'accesso (GNOME, KDE e gli altri desktop XDG).
pub fn add_autostart() -> Result<()> {
    write(
        &autostart_entry(),
        &format!(
            "[Desktop Entry]\nType=Application\nName=CYD Deck\nComment=Drives the CYD Deck macropad\nComment[it]=Comanda il macropad CYD Deck\n\
             Exec={} run\nIcon=cyd-deck\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\n",
            quoted_exe()
        ),
    )
}

pub fn autostart_enabled() -> bool {
    autostart_entry().exists()
}

/// Avvia il programma in background staccato da chi lo lancia: resta acceso anche chiudendo l'editor.
pub fn start_daemon() -> Result<()> {
    use std::os::unix::process::CommandExt;
    let log_dir = home().join(".cache/cyd-deck");
    std::fs::create_dir_all(&log_dir)?;
    let log = std::fs::File::create(log_dir.join("daemon.log"))?;
    Command::new(exe())
        .arg("run")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0)
        .spawn()
        .context("cyd-deck run")?;
    Ok(())
}

fn hyprland() -> bool {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
}

pub fn cli_install() -> Result<()> {
    add_to_menu()?;
    add_autostart()?;
    println!("{}", tr!("CYD Deck aggiunto al menu delle app e all'avvio ({})", exe().display()));
    if hyprland() {
        println!(
            "{}",
            tr!("Hyprland non legge l'avvio automatico standard: aggiungi a hyprland.conf la riga\n  exec-once = {} run", quoted_exe())
        );
    }
    Ok(())
}

pub fn cli_uninstall() -> Result<()> {
    for p in [menu_entry(), icon_file(), autostart_entry()] {
        match std::fs::remove_file(&p) {
            Ok(()) => println!("{}", tr!("rimosso {}", p.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => eprintln!("{}: {e}", p.display()),
        }
    }
    println!("{}", tr!("La config resta in ~/.config/cyd-deck: cancellala a mano se non ti serve più."));
    Ok(())
}
