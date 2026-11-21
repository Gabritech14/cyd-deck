use std::collections::BTreeMap;

use serde_json::Value;

use crate::app::{shell_output, spawn_shell};
use crate::config::MixerConfig;

/// A cosa si applica un cursore del mixer.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Sink,
    Source,
    /// Tutti gli stream di un'app (es. più schede del browser).
    Nodes(Vec<u64>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Channel {
    /// Identificativo stabile tra un aggiornamento e l'altro.
    pub key: String,
    pub name: String,
    pub icon: &'static str,
    /// Volume come lo mostrano wpctl e Caelestia (scala cubica, 1.0 = 100%).
    pub volume: f32,
    pub muted: bool,
    pub target: Target,
}

fn props(node: &Value) -> &Value {
    &node["info"]["props"]
}

fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// Icona Material plausibile per un'app audio.
pub fn app_icon(name: &str, binary: &str) -> &'static str {
    let n = format!("{name} {binary}").to_lowercase();
    let has = |w: &[&str]| w.iter().any(|x| n.contains(x));
    match () {
        _ if has(&["firefox", "chrom", "zen", "brave", "vivaldi", "librewolf", "edge", "opera"]) => "public",
        _ if has(&["discord", "vesktop", "webcord", "telegram", "teams", "slack", "zoom", "signal"]) => "forum",
        _ if has(&["spotify", "music", "limusic", "rhythmbox", "elisa", "amberol", "tidal", "deezer", "cider"]) => "music_note",
        _ if has(&["mpv", "vlc", "celluloid", "totem", "video", "jellyfin", "kodi"]) => "movie",
        _ if has(&["steam", "game", "minecraft", "java", "wine", "proton", "lutris", "heroic", ".exe"]) => "sports_esports",
        _ if has(&["obs"]) => "videocam",
        _ => "graphic_eq",
    }
}

fn node_volume(node: &Value) -> (f32, bool) {
    let list = node["info"]["params"]["Props"].as_array().cloned().unwrap_or_default();
    let p = list.iter().find(|p| p.get("channelVolumes").is_some()).cloned().unwrap_or(Value::Null);
    let lin = p["channelVolumes"].as_array().map(|v| v.iter().filter_map(Value::as_f64).fold(0.0, f64::max)).unwrap_or(1.0);
    // PipeWire salva il volume lineare; wpctl e Caelestia mostrano la radice cubica.
    ((lin.cbrt()) as f32, p["mute"].as_bool().unwrap_or(false))
}

/// Volume di @DEFAULT_AUDIO_SINK@ / SOURCE@ letto con wpctl ("Volume: 0.40 [MUTED]").
fn wpctl_volume(target: &str) -> Option<(f32, bool)> {
    let out = shell_output(&format!("wpctl get-volume {target}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let vol = text.split_whitespace().nth(1)?.parse().ok()?;
    Some((vol, text.contains("MUTED")))
}

fn default_device_name(dump: &[Value], key: &str) -> Option<String> {
    let meta = dump.iter().find(|o| o["props"]["metadata.name"] == "default")?;
    let entry = meta["metadata"].as_array()?.iter().find(|e| e["key"] == key)?;
    let name = entry["value"]["name"].as_str()?;
    let node = dump.iter().find(|o| s(props(o), "node.name") == Some(name))?;
    s(props(node), "node.description").or(s(props(node), "node.nick")).map(str::to_string)
}

fn device_icon(desc: &str, mic: bool) -> &'static str {
    let d = desc.to_lowercase();
    match () {
        _ if mic => "mic",
        _ if d.contains("hdmi") || d.contains("displayport") => "tv",
        _ if ["headphone", "headset", "cuffie", "buds", "tah", "wh-", "airpods"].iter().any(|w| d.contains(w)) => "headphones",
        _ => "volume_up",
    }
}

/// Costruisce i canali dal JSON di `pw-dump`. I volumi di uscita e microfono si passano a parte.
pub fn parse(dump: &[Value], cfg: &MixerConfig, sink: Option<(f32, bool)>, source: Option<(f32, bool)>) -> Vec<Channel> {
    let mut out = Vec::new();
    if cfg.master {
        if let Some((volume, muted)) = sink {
            let desc = default_device_name(dump, "default.audio.sink").unwrap_or_default();
            out.push(Channel {
                key: "@sink".into(),
                name: "Generale".into(),
                icon: device_icon(&desc, false),
                volume,
                muted,
                target: Target::Sink,
            });
        }
    }
    if cfg.mic {
        if let Some((volume, muted)) = source {
            out.push(Channel { key: "@source".into(), name: "Microfono".into(), icon: "mic", volume, muted, target: Target::Source });
        }
    }
    let hidden: Vec<String> = cfg.hide.iter().map(|h| h.trim().to_lowercase()).collect();
    let mut apps: BTreeMap<String, Channel> = BTreeMap::new();
    for node in dump {
        let p = props(node);
        if node["type"] != "PipeWire:Interface:Node" || s(p, "media.class") != Some("Stream/Output/Audio") {
            continue;
        }
        let Some(id) = node["id"].as_u64() else { continue };
        let binary = s(p, "application.process.binary").unwrap_or("");
        let Some(name) = s(p, "application.name").or(Some(binary).filter(|b| !b.is_empty())).or(s(p, "node.name")) else {
            continue;
        };
        if hidden.iter().any(|h| *h == name.to_lowercase() || (!binary.is_empty() && *h == binary.to_lowercase())) {
            continue;
        }
        let (volume, muted) = node_volume(node);
        let ch = apps.entry(name.to_lowercase()).or_insert_with(|| Channel {
            key: format!("app:{}", name.to_lowercase()),
            name: name.to_string(),
            icon: app_icon(name, binary),
            volume,
            muted,
            target: Target::Nodes(Vec::new()),
        });
        if let Target::Nodes(ids) = &mut ch.target {
            if !ids.is_empty() {
                ch.volume = ch.volume.max(volume);
                ch.muted &= muted;
            }
            ids.push(id);
        }
    }
    out.extend(apps.into_values());
    out
}

/// Se un canale va mostrato con le opzioni della pagina.
pub fn shown(ch: &Channel, cfg: &MixerConfig) -> bool {
    match ch.target {
        Target::Sink => cfg.master,
        Target::Source => cfg.mic,
        Target::Nodes(_) => !cfg.hide.iter().any(|h| h.trim().eq_ignore_ascii_case(&ch.name)),
    }
}

/// Stato attuale dei volumi (pw-dump + wpctl, qualche decina di ms).
pub fn snapshot(cfg: &MixerConfig) -> Vec<Channel> {
    let dump: Vec<Value> = shell_output("pw-dump")
        .and_then(|o| serde_json::from_slice(&o.stdout).ok())
        .unwrap_or_default();
    let sink = if cfg.master { wpctl_volume("@DEFAULT_AUDIO_SINK@") } else { None };
    let source = if cfg.mic { wpctl_volume("@DEFAULT_AUDIO_SOURCE@") } else { None };
    parse(&dump, cfg, sink, source)
}

fn ids(t: &Target) -> Vec<String> {
    match t {
        Target::Sink => vec!["@DEFAULT_AUDIO_SINK@".into()],
        Target::Source => vec!["@DEFAULT_AUDIO_SOURCE@".into()],
        Target::Nodes(n) => n.iter().map(u64::to_string).collect(),
    }
}

pub fn set_volume(t: &Target, volume: f32) {
    let v = volume.clamp(0.0, 1.0);
    let cmd: Vec<String> = ids(t).iter().map(|id| format!("wpctl set-volume {id} {v:.3}")).collect();
    spawn_shell(&cmd.join("; "));
}

pub fn set_mute(t: &Target, mute: bool) {
    let cmd: Vec<String> = ids(t).iter().map(|id| format!("wpctl set-mute {id} {}", u8::from(mute))).collect();
    spawn_shell(&cmd.join("; "));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_streams() {
        let dump: Vec<Value> = serde_json::from_str(
            r#"[
            {"id": 1, "type": "PipeWire:Interface:Metadata", "props": {"metadata.name": "default"},
             "metadata": [{"key": "default.audio.sink", "value": {"name": "out"}}]},
            {"id": 2, "type": "PipeWire:Interface:Node", "info": {"props": {"media.class": "Audio/Sink",
              "node.name": "out", "node.description": "Cuffie TAH5209"}, "params": {}}},
            {"id": 10, "type": "PipeWire:Interface:Node", "info": {"props": {"media.class": "Stream/Output/Audio",
              "application.name": "Firefox"}, "params": {"Props": [{"channelVolumes": [0.125, 0.064], "mute": false}]}}},
            {"id": 11, "type": "PipeWire:Interface:Node", "info": {"props": {"media.class": "Stream/Output/Audio",
              "application.name": "Firefox"}, "params": {"Props": [{"channelVolumes": [1.0], "mute": true}]}}},
            {"id": 12, "type": "PipeWire:Interface:Node", "info": {"props": {"media.class": "Stream/Output/Audio",
              "application.name": "Discord"}, "params": {"Props": [{"channelVolumes": [1.0]}]}}},
            {"id": 13, "type": "PipeWire:Interface:Node", "info": {"props": {"media.class": "Stream/Input/Audio",
              "application.name": "cava"}, "params": {}}}
        ]"#,
        )
        .unwrap();
        let cfg = MixerConfig { hide: vec!["discord".into()], ..MixerConfig::default() };
        let ch = parse(&dump, &cfg, Some((0.4, false)), None);
        assert_eq!(ch.len(), 2);
        assert_eq!((ch[0].name.as_str(), ch[0].icon, ch[0].volume), ("Generale", "headphones", 0.4));
        assert_eq!(ch[1].name, "Firefox");
        assert_eq!(ch[1].target, Target::Nodes(vec![10, 11]));
        assert!((ch[1].volume - 1.0).abs() < 1e-6 && !ch[1].muted);
        assert_eq!(ch[1].icon, "public");
    }
}
