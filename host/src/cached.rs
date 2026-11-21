//! `cyd-deck cached <minuti> <comando>`: stampa l'ultimo risultato del comando e lo rinnova in
//! background quando è più vecchio di N minuti. Serve ai tasti con etichette lente da calcolare
//! (aggiornamenti, meteo...), che il demone interroga ogni pochi secondi.

use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

fn cache_dir() -> PathBuf {
    // Mai in /tmp, condivisa con gli altri utenti: un file preparato da loro verrebbe sovrascritto.
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(run) => PathBuf::from(run).join("cyd-deck/cache"),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache/cyd-deck/cache"),
    }
}

fn age(p: &std::path::Path) -> Option<Duration> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok())
}

pub fn run(args: &[String]) -> ExitCode {
    let (Some(mins), Some(_)) = (args.first().and_then(|m| m.parse::<u64>().ok()), args.get(1)) else {
        eprintln!("uso: cyd-deck cached <minuti> <comando>");
        return ExitCode::FAILURE;
    };
    let cmd = args[1..].join(" ");
    let dir = cache_dir();
    let _ = std::fs::create_dir_all(&dir);
    use sha2::{Digest, Sha256};
    let name: String = Sha256::digest(cmd.as_bytes())[..8].iter().map(|b| format!("{b:02x}")).collect();
    let (out, lock) = (dir.join(&name), dir.join(format!("{name}.lock")));
    let stale = age(&out).is_none_or(|a| a > Duration::from_secs(mins * 60));
    // Un solo aggiornamento alla volta (il lucchetto scade dopo 5 minuti, se il comando si pianta).
    let running = age(&lock).is_some_and(|a| a < Duration::from_secs(300));
    if stale && !running {
        let _ = std::fs::write(&lock, "");
        let script = format!(
            "({cmd}) > '{tmp}' 2>/dev/null && mv '{tmp}' '{out}'; rm -f '{lock}'",
            tmp = dir.join(format!("{name}.tmp")).display(),
            out = out.display(),
            lock = lock.display()
        );
        let _ = Command::new("sh")
            .arg("-c")
            .arg(script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn();
    }
    match std::fs::read_to_string(&out) {
        Ok(text) => print!("{}", text.trim_end()),
        Err(_) => print!("…"),
    }
    ExitCode::SUCCESS
}
