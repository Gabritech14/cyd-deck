//! Esecuzione delle macro: passi in sequenza su un thread separato.

use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::app::{shell_output, spawn_shell};
use crate::config::{CloseScope, Step};

/// Le finestre con questa classe (l'editor) non vengono mai chiuse.
const OWN_CLASS: &str = "cyd-deck";
const CLOSE_TIMEOUT: Duration = Duration::from_secs(4);

/// Stringa Lua tra virgolette doppie.
fn lua_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Argomento shell tra apici singoli.
pub(crate) fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Comando che lancia `exec` tramite Hyprland con le finestre sul workspace dato, senza spostarsi.
pub fn exec_on_workspace(exec: &str, workspace: &str) -> String {
    let lua = format!("hl.dsp.exec_cmd({}, {{ workspace = {} }})", lua_str(exec), lua_str(&format!("{workspace} silent")));
    format!("hyprctl dispatch {}", sh_quote(&lua))
}

pub fn focus_workspace(workspace: &str) -> String {
    format!("hyprctl dispatch {}", sh_quote(&format!("hl.dsp.focus({{ workspace = {} }})", lua_str(workspace))))
}

fn hypr_json(what: &str) -> Option<Value> {
    let out = shell_output(&format!("hyprctl {what} -j"))?;
    serde_json::from_slice(&out.stdout).ok()
}

/// Sceglie le finestre da chiudere tra quelle restituite da `hyprctl clients -j`.
fn windows_to_close(clients: &Value, active_ws: i64, scope: CloseScope, workspace: Option<&str>, special: bool) -> Vec<String> {
    let Some(list) = clients.as_array() else { return Vec::new() };
    list.iter()
        .filter(|c| c["class"].as_str() != Some(OWN_CLASS))
        .filter(|c| {
            let id = c["workspace"]["id"].as_i64().unwrap_or(0);
            let name = c["workspace"]["name"].as_str().unwrap_or_default();
            // I workspace speciali hanno id negativo.
            let is_special = id < 0;
            match scope {
                CloseScope::Current => id == active_ws,
                CloseScope::Workspace => workspace.is_some_and(|w| w == name || w == id.to_string()),
                CloseScope::All => special || !is_special,
            }
        })
        .filter_map(|c| c["address"].as_str().map(str::to_string))
        .collect()
}

pub fn close_window(address: &str) -> String {
    format!("hyprctl dispatch {}", sh_quote(&format!("hl.dsp.window.close({{ window = {} }})", lua_str(&format!("address:{address}")))))
}

/// Chiede la chiusura delle finestre e aspetta (al massimo qualche secondo) che spariscano.
fn close_windows(scope: CloseScope, workspace: Option<&str>, special: bool) {
    let Some(clients) = hypr_json("clients") else { return };
    let active = hypr_json("activeworkspace").and_then(|w| w["id"].as_i64()).unwrap_or(0);
    let targets = windows_to_close(&clients, active, scope, workspace.map(str::trim), special);
    for a in &targets {
        shell_output(&close_window(a));
    }
    let start = Instant::now();
    while !targets.is_empty() && start.elapsed() < CLOSE_TIMEOUT {
        thread::sleep(Duration::from_millis(100));
        let still_open = hypr_json("clients").and_then(|c| c.as_array().cloned()).is_some_and(|list| {
            list.iter().any(|c| c["address"].as_str().is_some_and(|a| targets.iter().any(|t| t == a)))
        });
        if !still_open {
            break;
        }
    }
}

/// Esegue i passi in ordine, con `step_delay_ms` di pausa tra uno e l'altro.
/// Le azioni interne vengono passate a `on_action`.
pub fn run(steps: Vec<Step>, step_delay_ms: u64, on_action: impl Fn(String) + Send + 'static) {
    thread::spawn(move || {
        for (i, step) in steps.into_iter().enumerate() {
            if i > 0 && step_delay_ms > 0 {
                thread::sleep(Duration::from_millis(step_delay_ms.min(60_000)));
            }
            match step {
                Step::Exec { exec, workspace, background, wait } => {
                    let cmd = match workspace.as_deref().map(str::trim).filter(|w| !w.is_empty()) {
                        Some(ws) => {
                            // Prima ci spostiamo lì; la regola tiene comunque la finestra su `ws`
                            // anche se l'app si apre dopo che la macro è andata avanti.
                            if !background {
                                shell_output(&focus_workspace(ws));
                            }
                            exec_on_workspace(&exec, ws)
                        }
                        None => exec,
                    };
                    if wait {
                        shell_output(&cmd);
                    } else {
                        spawn_shell(&cmd);
                    }
                }
                // Aspettiamo hyprctl così il passo dopo parte già sul workspace giusto.
                Step::Workspace { workspace } => {
                    shell_output(&focus_workspace(workspace.trim()));
                }
                Step::Delay { ms } => thread::sleep(Duration::from_millis(ms.min(60_000))),
                Step::Action { action } => on_action(action),
                Step::CloseWindows { scope, workspace, special } => close_windows(scope, workspace.as_deref(), special),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(
            exec_on_workspace("spotify --uri \"x\"", "2"),
            r#"hyprctl dispatch 'hl.dsp.exec_cmd("spotify --uri \"x\"", { workspace = "2 silent" })'"#
        );
        assert_eq!(focus_workspace("1"), r#"hyprctl dispatch 'hl.dsp.focus({ workspace = "1" })'"#);
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(close_window("0xab"), r#"hyprctl dispatch 'hl.dsp.window.close({ window = "address:0xab" })'"#);
    }

    #[test]
    fn selects_windows() {
        let clients: Value = serde_json::from_str(
            r#"[
            {"address": "0x1", "class": "foot", "workspace": {"id": 1, "name": "1"}},
            {"address": "0x2", "class": "firefox", "workspace": {"id": 2, "name": "2"}},
            {"address": "0x3", "class": "spotify", "workspace": {"id": -98, "name": "special:music"}},
            {"address": "0x4", "class": "cyd-deck", "workspace": {"id": 1, "name": "1"}}
        ]"#,
        )
        .unwrap();
        let pick = |scope, ws, special| windows_to_close(&clients, 1, scope, ws, special);
        assert_eq!(pick(CloseScope::Current, None, false), ["0x1"]);
        assert_eq!(pick(CloseScope::Workspace, Some("2"), false), ["0x2"]);
        assert_eq!(pick(CloseScope::Workspace, Some("special:music"), false), ["0x3"]);
        assert_eq!(pick(CloseScope::All, None, false), ["0x1", "0x2"]);
        assert_eq!(pick(CloseScope::All, None, true), ["0x1", "0x2", "0x3"]);
    }
}
