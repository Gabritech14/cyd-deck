use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::process::Command;

use crate::app::{shell_output, spawn_shell};
use crate::macros::sh_quote;
use crate::render::Rgb;

/// Brano del lettore attivo.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NowPlaying {
    /// Nome del player per playerctl (es. "spotify").
    pub player: String,
    pub title: String,
    pub artist: String,
    pub art_url: String,
    pub playing: bool,
    /// Secondi.
    pub position: f32,
    pub length: f32,
}

/// Player scelto da Caelestia (stessa logica della sua scheda media), via IPC di Quickshell.
pub fn caelestia_player() -> Option<String> {
    let out = shell_output("qs -c caelestia ipc call mpris getActive identity")?;
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !id.is_empty() && id != "No active player").then_some(id)
}

fn parse_players(text: &str) -> Vec<NowPlaying> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            let [player, status, title, artist, art, pos, len] = f.as_slice() else { return None };
            let us = |s: &str| s.trim().parse::<f64>().map(|v| (v / 1e6) as f32).unwrap_or(0.0);
            Some(NowPlaying {
                player: player.to_string(),
                playing: *status == "Playing",
                title: title.to_string(),
                artist: artist.to_string(),
                art_url: art.to_string(),
                position: us(pos),
                length: us(len),
            })
        })
        .collect()
}

/// Sceglie il player che corrisponde a quello attivo di Caelestia, altrimenti
/// quello in riproduzione, altrimenti il primo.
fn pick(players: Vec<NowPlaying>, identity: Option<&str>) -> Option<NowPlaying> {
    let norm = |s: &str| s.to_lowercase().replace([' ', '-', '_'], "");
    if let Some(id) = identity.map(norm) {
        if let Some(p) = players.iter().find(|p| {
            let n = norm(p.player.split('.').next().unwrap_or(&p.player));
            n == id || n.contains(&id) || id.contains(&n)
        }) {
            return Some(p.clone());
        }
    }
    players.iter().find(|p| p.playing).or(players.first()).cloned()
}

pub fn query(identity: Option<&str>) -> Option<NowPlaying> {
    let fmt = "{{playerName}}\t{{status}}\t{{title}}\t{{artist}}\t{{mpris:artUrl}}\t{{position}}\t{{mpris:length}}";
    let out = shell_output(&format!("playerctl -a metadata --format {}", sh_quote(fmt)))?;
    pick(parse_players(&String::from_utf8_lossy(&out.stdout)), identity)
}

/// `cmd`: play-pause, next, previous.
pub fn control(player: &str, cmd: &str) {
    if player.is_empty() {
        let ipc = match cmd {
            "play-pause" => "playPause",
            c => c,
        };
        spawn_shell(&format!("caelestia shell mpris {ipc}"));
    } else {
        spawn_shell(&format!("playerctl -p {} {cmd}", sh_quote(player)));
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Scarica (se serve) e ridimensiona la copertina a `size`×`size`, ritagliata al centro.
pub fn load_art(url: &str, size: u32) -> Option<Vec<Rgb>> {
    let src = if let Some(path) = url.strip_prefix("file://") {
        PathBuf::from(percent_decode(path))
    } else if url.starts_with("http://") || url.starts_with("https://") {
        let dir = PathBuf::from(std::env::var_os("HOME")?).join(".cache/cyd-deck/art");
        std::fs::create_dir_all(&dir).ok()?;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        url.hash(&mut h);
        let file = dir.join(format!("{:016x}", h.finish()));
        if !file.exists() {
            let tmp = file.with_extension("part");
            let ok = Command::new("curl")
                .args(["-sfL", "--max-time", "8", "-o"])
                .arg(&tmp)
                .arg(url)
                .status()
                .is_ok_and(|s| s.success());
            if !ok || std::fs::rename(&tmp, &file).is_err() {
                let _ = std::fs::remove_file(&tmp);
                return None;
            }
        }
        file
    } else {
        return None;
    };
    let out = Command::new("magick")
        .arg(format!("{}[0]", src.display()))
        .args(["-resize", &format!("{size}x{size}^"), "-gravity", "center", "-extent", &format!("{size}x{size}")])
        .args(["-depth", "8", "rgb:-"])
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.len() != (size * size * 3) as usize {
        return None;
    }
    Some(out.stdout.chunks(3).map(|p| [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0]).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_caelestia_player() {
        let text = "firefox.instance_1_2\tPlaying\tVideo\t\t\t1000000\t\n\
                    spotify\tPaused\tSong\tArtist\thttps://x/y\t5000000\t200000000\n";
        let players = parse_players(text);
        assert_eq!(players.len(), 2);
        let p = pick(players.clone(), Some("Spotify")).unwrap();
        assert_eq!((p.title.as_str(), p.playing, p.position, p.length), ("Song", false, 5.0, 200.0));
        assert_eq!(pick(players.clone(), Some("Mozilla Firefox")).unwrap().player, "firefox.instance_1_2");
        assert_eq!(pick(players, None).unwrap().title, "Video");
        assert_eq!(percent_decode("/a%20b/c%C3%A8.jpg"), "/a b/cè.jpg");
    }
}
