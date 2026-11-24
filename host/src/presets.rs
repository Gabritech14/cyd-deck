use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::config::Key;
use crate::i18n::t;

/// Azione pronta da assegnare a un tasto dall'editor.
pub struct Preset {
    pub name: &'static str,
    pub icon: &'static str,
    pub label: &'static str,
    pub exec: &'static str,
    pub state: Option<&'static str>,
    pub icon_active: Option<&'static str>,
    pub label_cmd: Option<&'static str>,
    /// Spiegazione mostrata nello store.
    pub desc: &'static str,
    /// Azione interna del deck al posto di un comando.
    pub action: Option<&'static str>,
}

const P: Preset = Preset { name: "", icon: "", label: "", exec: "", state: None, icon_active: None, label_cmd: None, desc: "", action: None };

const fn p(name: &'static str, icon: &'static str, label: &'static str, exec: &'static str) -> Preset {
    Preset { name, icon, label, exec, ..P }
}

/// Preset che esegue un'azione interna del deck.
const fn a(name: &'static str, icon: &'static str, label: &'static str, action: &'static str, desc: &'static str) -> Preset {
    Preset { name, icon, label, action: Some(action), desc, ..P }
}

impl Preset {
    pub fn to_key(&self) -> Key {
        Key {
            icon: Some(self.icon.into()),
            icon_active: self.icon_active.map(Into::into),
            label: Some(crate::i18n::t(self.label).into()),
            exec: (!self.exec.is_empty()).then(|| self.exec.into()),
            action: self.action.map(Into::into),
            state: self.state.map(Into::into),
            label_cmd: self.label_cmd.map(Into::into),
            ..Key::default()
        }
    }
}

pub struct Group {
    pub name: &'static str,
    pub presets: &'static [Preset],
}

pub const GROUPS: &[Group] = &[
    Group {
        name: "Caelestia",
        presets: &[
            p("Launcher", "apps", "Launcher", "caelestia shell drawers toggle launcher"),
            p("Dashboard", "dashboard", "Dashboard", "caelestia shell drawers toggle dashboard"),
            p("Sidebar notifiche", "notifications", "Notifiche", "caelestia shell drawers toggle sidebar"),
            p("Utilità", "widgets", "Utilità", "caelestia shell drawers toggle utilities"),
            p("Menu sessione", "power_settings_new", "Sessione", "caelestia shell drawers toggle session"),
            p("Blocca schermo", "lock", "Blocca", "caelestia shell lock lock"),
            p("Screenshot area", "screenshot_region", "Screenshot", "caelestia shell picker openFreeze"),
            p("Screenshot negli appunti", "content_cut", "Ritaglia", "caelestia shell picker openFreezeClip"),
            p("Screenshot schermo intero", "screenshot_monitor", "Schermo", "caelestia screenshot"),
            p("Registra schermo", "videocam", "Registra", "caelestia record -s"),
            p("Registra area", "screen_record", "Reg. area", "caelestia record -r -s"),
            p("Pausa registrazione", "pause_circle", "Pausa reg.", "caelestia record -p"),
            p("Cronologia appunti", "content_paste", "Appunti", "caelestia clipboard"),
            p("Emoji", "mood", "Emoji", "caelestia emoji -p"),
            p("Sfondo casuale", "wallpaper", "Sfondo", "caelestia wallpaper -r"),
            Preset {
                state: Some(r#"[ "$(caelestia shell notifs isDndEnabled)" = true ]"#),
                ..p("Non disturbare", "do_not_disturb_on", "Non disturb.", "caelestia shell notifs toggleDnd")
            },
            Preset {
                state: Some(r#"[ "$(caelestia shell gameMode isEnabled)" = true ]"#),
                ..p("Game mode", "sports_esports", "Game mode", "caelestia shell gameMode toggle")
            },
            p("Game mode: attiva", "sports_esports", "Game ON", "caelestia shell gameMode enable"),
            p("Game mode: disattiva", "sports_esports", "Game OFF", "caelestia shell gameMode disable"),
            p("Non disturbare: attiva", "do_not_disturb_on", "DND ON", "caelestia shell notifs enableDnd"),
            p("Non disturbare: disattiva", "do_not_disturb_off", "DND OFF", "caelestia shell notifs disableDnd"),
            Preset {
                state: Some(r#"[ "$(caelestia shell idleInhibitor isEnabled)" = true ]"#),
                ..p("Inibisci sospensione", "coffee", "Sempre on", "caelestia shell idleInhibitor toggle")
            },
            p("Cancella notifiche", "clear_all", "Pulisci", "caelestia shell notifs clear"),
            p("Workspace musica", "music_note", "Musica", "caelestia toggle music"),
            p("Workspace social", "chat", "Social", "caelestia toggle communication"),
            p("Workspace sysmon", "monitoring", "Sysmon", "caelestia toggle sysmon"),
            p("Workspace todo", "checklist", "Todo", "caelestia toggle todo"),
        ],
    },
    Group {
        name: "Media e audio",
        presets: &[
            Preset {
                state: Some(r#"[ "$(playerctl status 2>/dev/null)" = Playing ]"#),
                icon_active: Some("pause"),
                ..p("Play / pausa", "play_arrow", "Play", "caelestia shell mpris playPause")
            },
            p("Brano precedente", "skip_previous", "Indietro", "caelestia shell mpris previous"),
            p("Brano successivo", "skip_next", "Avanti", "caelestia shell mpris next"),
            p("Stop", "stop", "Stop", "caelestia shell mpris stop"),
            Preset {
                state: Some("wpctl get-volume @DEFAULT_AUDIO_SINK@ | grep -q MUTED"),
                icon_active: Some("volume_off"),
                label_cmd: Some(r#"wpctl get-volume @DEFAULT_AUDIO_SINK@ | awk '{printf "%d%%", $2*100}'"#),
                ..p("Muto (con volume)", "volume_up", "Muto", "wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle")
            },
            p("Volume +5%", "volume_up", "Vol +", "wpctl set-volume -l 1.0 @DEFAULT_AUDIO_SINK@ 5%+"),
            p("Volume -5%", "volume_down", "Vol -", "wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%-"),
            Preset {
                state: Some("wpctl get-volume @DEFAULT_AUDIO_SOURCE@ | grep -q MUTED"),
                icon_active: Some("mic_off"),
                ..p("Microfono muto", "mic", "Microfono", "wpctl set-mute @DEFAULT_AUDIO_SOURCE@ toggle")
            },
            p("Cambia uscita audio", "speaker", "Uscita", "caelestia shell audio cycleOutput"),
            Preset {
                state: Some(r#"[ "$(playerctl shuffle 2>/dev/null)" = On ]"#),
                ..p("Shuffle", "shuffle", "Shuffle", "playerctl shuffle toggle")
            },
            Preset {
                label_cmd: Some(r#"playerctl metadata --format '{{ title }}' 2>/dev/null"#),
                desc: r#"Titolo del brano sul tasto; premi per play/pausa"#,
                ..p("Brano attuale", "music_note", "Brano", r#"playerctl play-pause"#)
            },
            p("Avanti 10 secondi", "forward_10", "+10 s", r#"playerctl position 10+"#),
            p("Indietro 10 secondi", "replay_10", "-10 s", r#"playerctl position 10-"#),
            Preset {
                state: Some(r#"wpctl get-volume @DEFAULT_AUDIO_SOURCE@ | grep -q MUTED"#),
                icon_active: Some("mic_off"),
                label_cmd: Some(r#"wpctl get-volume @DEFAULT_AUDIO_SOURCE@ | awk '{printf "%d%%", $2*100}'"#),
                ..p("Volume microfono", "mic", "Mic", r#"wpctl set-mute @DEFAULT_AUDIO_SOURCE@ toggle"#)
            },
        ],
    },
    Group {
        name: "Hyprland",
        presets: &[
            p("Chiudi finestra", "close", "Chiudi", "hyprctl dispatch 'hl.dsp.window.close()'"),
            p("Schermo intero", "fullscreen", "Fullscreen", r#"hyprctl dispatch 'hl.dsp.window.fullscreen({ mode = "fullscreen" })'"#),
            p("Massimizza", "open_in_full", "Massimizza", r#"hyprctl dispatch 'hl.dsp.window.fullscreen({ mode = "maximized" })'"#),
            p("Finestra flottante", "picture_in_picture", "Flottante", "hyprctl dispatch 'hl.dsp.window.float()'"),
            p("Fissa finestra", "push_pin", "Pin", "hyprctl dispatch 'hl.dsp.window.pin()'"),
            p("Workspace precedente", "arrow_back", "WS -", r#"hyprctl dispatch 'hl.dsp.focus({ workspace = "-1" })'"#),
            p("Workspace successivo", "arrow_forward", "WS +", r#"hyprctl dispatch 'hl.dsp.focus({ workspace = "+1" })'"#),
            p("Workspace 1", "counter_1", "WS 1", r#"hyprctl dispatch 'hl.dsp.focus({ workspace = "1" })'"#),
            p("Workspace 2", "counter_2", "WS 2", r#"hyprctl dispatch 'hl.dsp.focus({ workspace = "2" })'"#),
            p("Workspace 3", "counter_3", "WS 3", r#"hyprctl dispatch 'hl.dsp.focus({ workspace = "3" })'"#),
            p("Workspace 4", "counter_4", "WS 4", r#"hyprctl dispatch 'hl.dsp.focus({ workspace = "4" })'"#),
            p("Color picker", "colorize", "Colore", "hyprpicker -a"),
            Preset {
                label_cmd: Some(r#"hyprctl activewindow -j | jq -r '.class // empty'"#),
                ..p("Finestra attiva", "web_asset", "Finestra", r#""#)
            },
            Preset {
                label_cmd: Some(r#"hyprctl activeworkspace -j | jq -r .id"#),
                ..p("Workspace attuale", "grid_view", "WS", r#""#)
            },
            p("Ricarica Hyprland", "refresh", "Ricarica", r#"hyprctl reload"#),
        ],
    },
    Group {
        name: "Energia e hardware",
        presets: &[
            Preset {
                label_cmd: Some(r#"powerprofilesctl get | sed 's/power-saver/Risparmio/;s/balanced/Bilanciato/;s/performance/Turbo/'"#),
                desc: r#"Passa da risparmio a bilanciato a prestazioni; l'etichetta mostra quello attivo"#,
                ..p("Profilo energetico (cambia)", "speed", "Profilo", r#"case "$(powerprofilesctl get)" in power-saver) p=balanced;; balanced) p=performance;; *) p=power-saver;; esac; powerprofilesctl set $p"#)
            },
            Preset {
                state: Some(r#"[ "$(powerprofilesctl get)" = power-saver ]"#),
                ..p("Risparmio energetico", "eco", "Risparmio", r#"powerprofilesctl set power-saver"#)
            },
            Preset {
                state: Some(r#"[ "$(powerprofilesctl get)" = performance ]"#),
                ..p("Prestazioni massime", "rocket_launch", "Turbo", r#"powerprofilesctl set performance"#)
            },
            Preset {
                state: Some(r#"grep -q Charging /sys/class/power_supply/BAT0/status"#),
                icon_active: Some("battery_charging_full"),
                label_cmd: Some(r#"echo "$(cat /sys/class/power_supply/BAT0/capacity)%""#),
                desc: r#"Carica del portatile; si illumina mentre è in carica"#,
                ..p("Batteria del PC", "battery_full", "Batteria", r#""#)
            },
            Preset {
                label_cmd: Some(r#"sensors coretemp-isa-0000 | awk '/^Package/{gsub(/[^0-9.]/,"",$4); printf "%d°", $4}'"#),
                ..p("Temperatura CPU", "device_thermostat", "CPU", r#"foot -e btop"#)
            },
            Preset {
                label_cmd: Some(r#"nvidia-smi --query-gpu=temperature.gpu,power.draw --format=csv,noheader,nounits | awk -F', ' '{printf "%d° %dW", $1, $2}'"#),
                desc: r#"GPU NVIDIA: temperatura e consumo"#,
                ..p("GPU NVIDIA", "memory", "GPU", r#"foot -e watch -n1 nvidia-smi"#)
            },
            Preset {
                label_cmd: Some(r#"LC_ALL=C top -bn1 | awk '/^%Cpu/{printf "%d%%", 100-$8}'"#),
                ..p("Uso CPU", "developer_board", "CPU", r#"foot -e btop"#)
            },
            Preset {
                label_cmd: Some(r#"LC_ALL=C free | awk '/^Mem:/{printf "%d%%", $3*100/$2}'"#),
                ..p("Memoria RAM", "memory_alt", "RAM", r#"foot -e btop"#)
            },
            Preset {
                label_cmd: Some(r#"sensors thinkpad-isa-0000 | awk '/^fan1/{print $2" rpm"}'"#),
                ..p("Ventola", "mode_fan", "Ventola", r#""#)
            },
            Preset {
                label_cmd: Some(r#"df -h --output=avail / | tail -1 | tr -d ' '"#),
                ..p("Spazio libero su disco", "hard_drive", "Disco", r#"foot -H -e df -h"#)
            },
            Preset {
                label_cmd: Some(r#"brightnessctl -m | cut -d, -f4"#),
                ..p("Luminosità +10%", "brightness_high", "Lum +", r#"brightnessctl -q set +10%"#)
            },
            p("Luminosità -10%", "brightness_low", "Lum -", r#"brightnessctl -q set 10%-"#),
            Preset {
                state: Some(r#"pgrep -x hyprsunset >/dev/null"#),
                desc: r#"Schermo più caldo la sera (hyprsunset a 4000 K)"#,
                ..p("Luce notturna", "nightlight", "Notte", r#"pkill -x hyprsunset || (setsid hyprsunset -t 4000 >/dev/null 2>&1 &)"#)
            },
        ],
    },
    Group {
        name: "Rete e telefono",
        presets: &[
            Preset {
                state: Some(r#"bluetoothctl show | grep -q 'Powered: yes'"#),
                ..p("Bluetooth", "bluetooth", "Bluetooth", r#"bluetoothctl show | grep -q 'Powered: yes' && bluetoothctl power off || bluetoothctl power on"#)
            },
            Preset {
                label_cmd: Some(r#"nmcli -t -f NAME,DEVICE connection show --active | awk -F: '$2!="lo" && $2!~/^(docker|br-|veth)/ && $1!="cyd-hotspot"{print $1; exit}'"#),
                ..p("Rete attuale", "lan", "Rete", r#""#)
            },
            Preset {
                label_cmd: Some(r#"ip route get 1.1.1.1 2>/dev/null | awk '{for(i=1;i<NF;i++) if($i=="src"){print $(i+1); exit}}'"#),
                ..p("IP locale", "router", "IP", r#""#)
            },
            Preset {
                label_cmd: Some(r#"cyd-deck cached 30 curl -s -m5 https://ifconfig.me"#),
                desc: r#"Aggiornato ogni 30 minuti"#,
                ..p("IP pubblico", "public", "IP pubbl.", r#""#)
            },
            Preset {
                state: Some(r#"ping -c1 -W1 1.1.1.1 >/dev/null"#),
                label_cmd: Some(r#"ping -c1 -W1 1.1.1.1 | awk -F'time=' '/time=/{printf "%d ms", $2}'"#),
                desc: r#"Latenza verso 1.1.1.1; acceso se internet risponde"#,
                ..p("Ping internet", "network_check", "Ping", r#""#)
            },
            Preset {
                state: Some(r#"nmcli -t -f NAME connection show --active | grep -qx cyd-hotspot"#),
                desc: r#"Acceso se l'hotspot cyd-deck-ap è attivo (solo stato)"#,
                ..p("Hotspot del deck", "wifi_tethering", "Hotspot", r#""#)
            },
            Preset {
                desc: r#"Fa squillare il telefono con KDE Connect"#,
                ..p("Trova il telefono", "phonelink_ring", "Trova tel.", r#"kdeconnect-cli -d "$(kdeconnect-cli -a --id-only | head -1)" --ring"#)
            },
            p("Invia appunti al telefono", "send_to_mobile", "Al tel.", r#"kdeconnect-cli -d "$(kdeconnect-cli -a --id-only | head -1)" --share-text "$(wl-paste)""#),
            Preset {
                state: Some(r#"id=$(kdeconnect-cli -a --id-only | head -1); [ -n "$id" ] && busctl --user get-property org.kde.kdeconnect /modules/kdeconnect/devices/$id/battery org.kde.kdeconnect.device.battery isCharging | grep -q true"#),
                label_cmd: Some(r#"id=$(kdeconnect-cli -a --id-only | head -1); [ -n "$id" ] && busctl --user get-property org.kde.kdeconnect /modules/kdeconnect/devices/$id/battery org.kde.kdeconnect.device.battery charge | awk '{print $2"%"}'"#),
                desc: r#"Via KDE Connect; si illumina mentre carica"#,
                ..p("Batteria del telefono", "smartphone", "Telefono", r#""#)
            },
        ],
    },
    Group {
        name: "Aggiornamenti",
        presets: &[
            Preset {
                label_cmd: Some(r#"cyd-deck cached 30 '(checkupdates; paru -Qua) 2>/dev/null | wc -l'"#),
                desc: r#"Pacchetti da aggiornare (repo + AUR), ricontrollati ogni 30 minuti; premi per aggiornare"#,
                ..p("Aggiornamenti disponibili", "system_update", "Update", r#"foot -e paru"#)
            },
            p("Aggiorna il sistema", "upgrade", "Aggiorna", r#"foot -e paru"#),
            p("Aggiorna i Flatpak", "update", "Flatpak", r#"foot -e flatpak update"#),
            Preset {
                label_cmd: Some(r#"cyd-deck cached 60 'pacman -Qdtq | wc -l'"#),
                ..p("Pacchetti orfani", "delete_sweep", "Orfani", r#"foot -H -e pacman -Qdt"#)
            },
            p("Pulisci la cache dei pacchetti", "cleaning_services", "Pulisci", r#"foot -e paru -Sc"#),
            Preset {
                desc: r#"Da leggere prima di un aggiornamento grosso"#,
                ..p("Notizie di Arch Linux", "newspaper", "News Arch", r#"xdg-open https://archlinux.org/news/"#)
            },
        ],
    },
    Group {
        name: "Sviluppo e AI",
        presets: &[
            p("VS Code", "code", "VS Code", r#"code"#),
            Preset {
                state: Some(r#"curl -s -o /dev/null -m1 http://localhost:8080"#),
                desc: r#"Acceso se il container risponde"#,
                ..p("Open WebUI", "smart_toy", "WebUI", r#"xdg-open http://localhost:8080"#)
            },
            Preset {
                state: Some(r#"curl -s -o /dev/null -m1 http://localhost:8888"#),
                ..p("SearXNG", "travel_explore", "SearXNG", r#"xdg-open http://localhost:8888"#)
            },
            p("Cerca gli appunti su SearXNG", "manage_search", "Cerca", r#"xdg-open "http://localhost:8888/search?q=$(wl-paste | jq -sRr @uri)""#),
            Preset {
                state: Some(r#"[ "$(curl -s -m1 localhost:11434/api/ps | jq '.models|length')" -gt 0 ] 2>/dev/null"#),
                label_cmd: Some(r#"curl -s -m1 localhost:11434/api/ps | jq '.models|length'"#),
                desc: r#"Mostra quanti modelli sono in memoria; premi per scaricarli e liberare VRAM"#,
                ..p("Ollama: libera la memoria", "psychology", "Ollama", r#"ollama ps | awk 'NR>1{print $1}' | xargs -r -n1 ollama stop"#)
            },
            p("Alpaca", "chat", "Alpaca", r#"flatpak run com.jeffser.Alpaca"#),
            Preset {
                label_cmd: Some(r#"docker ps -q | wc -l"#),
                ..p("Container Docker attivi", "inventory_2", "Docker", r#"foot -H -e docker ps"#)
            },
            p("Riavvia Open WebUI", "restart_alt", "Riavvia UI", r#"docker restart open-webui"#),
        ],
    },
    Group {
        name: "Utilità",
        presets: &[
            Preset {
                desc: r#"Seleziona un'area: il testo finisce negli appunti (inglese; per l'italiano installa tesseract-data-ita)"#,
                ..p("Testo dallo schermo (OCR)", "document_scanner", "OCR", r#"grim -g "$(slurp)" - | tesseract stdin stdout -l eng 2>/dev/null | wl-copy && notify-send OCR "Testo copiato negli appunti""#)
            },
            Preset {
                state: Some(r#"[ -f "$XDG_RUNTIME_DIR/cyd-deck/pomodoro" ]"#),
                label_cmd: Some(r#"f="$XDG_RUNTIME_DIR/cyd-deck/pomodoro"; [ -f "$f" ] && r=$(( $(cat "$f") - $(date +%s) )) && [ $r -gt 0 ] && printf "%d:%02d" $((r/60)) $((r%60))"#),
                desc: r#"Conto alla rovescia sul tasto e notifica alla fine; premi di nuovo per annullare"#,
                ..p("Pomodoro 25 minuti", "timer", "Pomodoro", r#"f="$XDG_RUNTIME_DIR/cyd-deck/pomodoro"; if [ -f "$f" ]; then rm "$f"; notify-send Pomodoro Annullato; else end=$(( $(date +%s) + 1500 )); echo $end > "$f"; notify-send Pomodoro "25 minuti, concentrati!"; (sleep 1500; [ "$(cat "$f" 2>/dev/null)" = "$end" ] && rm "$f" && notify-send -u critical Pomodoro "Fai una pausa!") >/dev/null 2>&1 & fi"#)
            },
            p("Timer 5 minuti", "hourglass_top", "5 min", r#"notify-send Timer "5 minuti: partito"; sleep 300; notify-send -u critical Timer "Tempo scaduto!""#),
            Preset {
                label_cmd: Some(r#"cyd-deck cached 30 "curl -s -m5 'wttr.in/?format=%t' | tr -d +""#),
                desc: r#"Temperatura attuale (aggiornata ogni 30 minuti); premi per le previsioni"#,
                ..p("Meteo", "partly_cloudy_day", "Meteo", r#"foot -H -e curl -s 'wttr.in/?lang=it'"#)
            },
            p("Calcolatrice", "calculate", "Calcola", r#"foot -e qalc"#),
            p("Calendario", "calendar_month", "Calendario", r#"foot -H -e cal -3"#),
            p("Svuota appunti", "content_paste_off", "Svuota", r#"cliphist wipe && wl-copy --clear && notify-send Appunti Svuotati"#),
            p("Traduci gli appunti", "translate", "Traduci", r#"xdg-open "https://translate.google.com/?sl=auto&tl=it&text=$(wl-paste | jq -sRr @uri)""#),
            p("Apri il link negli appunti", "link", "Apri link", r#"xdg-open "$(wl-paste)""#),
            p("Informazioni di sistema", "info", "Info", r#"foot -H -e fastfetch"#),
            p("Log di sistema", "receipt_long", "Log", r#"foot -e journalctl -f"#),
            p("File", "folder", "File", r#"nautilus"#),
            p("Cartella Download", "download", "Download", r#"nautilus ~/Downloads"#),
            Preset {
                label_cmd: Some(r#"uptime -p | sed 's/up //;s/ days\?/g/;s/ hours\?/h/;s/ minutes\?/m/;s/,//g'"#),
                ..p("Tempo acceso", "timelapse", "Acceso", r#""#)
            },
        ],
    },
    Group {
        name: "App e giochi",
        presets: &[
            p("Firefox", "language", "Firefox", r#"firefox"#),
            p("Firefox privato", "visibility_off", "Privato", r#"firefox --private-window"#),
            p("Spotify", "music_note", "Spotify", r#"spotify"#),
            p("Discord", "forum", "Discord", r#"discord"#),
            p("Steam", "sports_esports", "Steam", r#"steam"#),
            p("Steam Big Picture", "tv", "Big Picture", r#"steam steam://open/bigpicture"#),
            p("Heroic Games Launcher", "videogame_asset", "Heroic", r#"flatpak run com.heroicgameslauncher.hgl"#),
            p("Cemu (Wii U)", "videogame_asset", "Cemu", r#"flatpak run info.cemu.Cemu"#),
            p("Ryujinx (Switch)", "videogame_asset", "Ryujinx", r#"flatpak run io.github.ryubing.Ryujinx"#),
            p("Bambu Studio", "view_in_ar", "Bambu", r#"flatpak run com.bambulab.BambuStudio"#),
            p("OpenSCAD", "deployed_code", "OpenSCAD", r#"openscad"#),
            p("LibreOffice", "description", "Office", r#"flatpak run org.libreoffice.LibreOffice"#),
            p("VLC", "smart_display", "VLC", r#"vlc"#),
        ],
    },
    Group {
        name: "Stampante 3D",
        presets: &[
            Preset {
                label_cmd: Some("cyd-deck printer progress"),
                state: Some("cyd-deck printer is-printing"),
                ..p("Avanzamento stampa", "print", "Stampa", "")
            },
            Preset { label_cmd: Some("cyd-deck printer remaining"), ..p("Tempo rimanente", "schedule", "Tempo", "") },
            Preset { label_cmd: Some("cyd-deck printer temps"), ..p("Temperature ugello / piatto", "thermostat", "Temp.", "") },
            Preset { label_cmd: Some("cyd-deck printer layer"), ..p("Strato attuale", "layers", "Strato", "") },
            Preset {
                state: Some("cyd-deck printer is-paused"),
                icon_active: Some("play_arrow"),
                ..p("Pausa stampa", "pause", "Pausa", "cyd-deck printer pause")
            },
            p("Riprendi stampa", "play_arrow", "Riprendi", "cyd-deck printer resume"),
            p("Ferma stampa (doppio tocco)", "stop_circle", "Stop x2", "cyd-deck printer stop"),
            Preset {
                state: Some("cyd-deck printer is-light"),
                ..p("Luce stampante", "lightbulb", "Luce", "cyd-deck printer light")
            },
            Preset {
                state: Some(r#"cyd-deck printer is-printing"#),
                label_cmd: Some(r#"cyd-deck printer state"#),
                ..p("Stato della stampante", "print", "Stampante", r#""#)
            },
            Preset {
                label_cmd: Some(r#"cyd-deck printer file"#),
                ..p("File in stampa", "description", "File", r#""#)
            },
            p("Apri Bambu Studio", "view_in_ar", "Bambu", r#"flatpak run com.bambulab.BambuStudio"#),
        ],
    },
    Group {
        name: "Sistema",
        presets: &[
            p("Terminale", "terminal", "Terminale", "foot"),
            p("Claude Code", "smart_toy", "Claude", "foot -e claude"),
            p("Monitor risorse", "monitor_heart", "btop", "foot -e btop"),
            p("Sospendi", "bedtime", "Sospendi", "systemctl suspend"),
            p("Riavvia il PC", "restart_alt", "Riavvia", "systemctl reboot"),
            p("Spegni il PC", "power_settings_new", "Spegni", "systemctl poweroff"),
            p("Esci dalla sessione", "logout", "Esci", r#"loginctl terminate-session "$XDG_SESSION_ID""#),
            p("Apri URL", "public", "Web", "xdg-open https://"),
            p("Batteria del deck", "{battery}", "{battery}", ""),
            a("Spegni il deck", "power_settings_new", "Spegni deck", "board-off", "Mette la scheda a riposo: si riaccende toccando lo schermo o con BOOT"),
            a("Riavvia il deck", "restart_alt", "Riavvia deck", "board-restart", "Riavvia la scheda, che poi si ricollega da sola"),
            a("Spegni lo schermo del deck", "dark_mode", "Off schermo", "backlight-off", "Spegne solo lo schermo: il primo tocco lo riaccende"),
        ],
    },
];

/// Categoria dello store con le app installate.
pub const APPS_CATEGORY: &str = "Applicazioni";

/// Categoria dei plugin scritti dall'utente che non ne indicano una.
pub const USER_CATEGORY: &str = "I miei plugin";

/// Funzione dello store: un preset integrato, un'app installata o un plugin dell'utente.
#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub struct Plugin {
    pub name: String,
    pub category: String,
    pub description: String,
    pub icon: String,
    pub label: String,
    pub exec: String,
    pub state: Option<String>,
    pub icon_active: Option<String>,
    pub label_cmd: Option<String>,
    /// Azione interna del deck (board-off, next-page, ...) al posto del comando.
    pub action: Option<String>,
    /// Viene da un file in plugins/.
    #[serde(skip)]
    pub user: bool,
}

impl Plugin {
    pub fn to_key(&self) -> Key {
        Key {
            icon: (!self.icon.is_empty()).then(|| self.icon.clone()),
            icon_active: self.icon_active.clone(),
            label: Some(if self.label.is_empty() { self.name.clone() } else { self.label.clone() }),
            exec: (!self.exec.is_empty()).then(|| self.exec.clone()),
            state: self.state.clone(),
            label_cmd: self.label_cmd.clone(),
            action: self.action.clone(),
            ..Key::default()
        }
    }

    /// Testo in cui cercare: nome, categoria, descrizione e comando.
    pub fn matches(&self, q: &str) -> bool {
        q.split_whitespace().all(|w| {
            [&self.name, &self.category, &self.description, &self.exec].iter().any(|t| t.to_lowercase().contains(w))
        })
    }
}

impl From<(&Group, &Preset)> for Plugin {
    fn from((g, p): (&Group, &Preset)) -> Self {
        Plugin {
            name: t(p.name).into(),
            category: t(g.name).into(),
            icon: p.icon.into(),
            label: t(p.label).into(),
            exec: p.exec.into(),
            state: p.state.map(Into::into),
            icon_active: p.icon_active.map(Into::into),
            label_cmd: p.label_cmd.map(Into::into),
            description: t(p.desc).into(),
            action: p.action.map(Into::into),
            ..Plugin::default()
        }
    }
}

#[derive(Deserialize)]
struct PluginFile {
    #[serde(default)]
    plugin: Vec<Plugin>,
}

/// Cartella dei plugin dell'utente, accanto alla config.
pub fn plugins_dir(cfg_path: &Path) -> PathBuf {
    cfg_path.parent().unwrap_or(Path::new(".")).join("plugins")
}

/// Preset integrati più i plugin in `dir/*.toml`; i file illeggibili finiscono negli errori.
pub fn catalog(dir: &Path) -> (Vec<Plugin>, Vec<String>) {
    let mut out: Vec<Plugin> = GROUPS.iter().flat_map(|g| g.presets.iter().map(move |p| Plugin::from((g, p)))).collect();
    let mut errors = Vec::new();
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "toml")).collect())
        .unwrap_or_default();
    files.sort();
    for path in files {
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        let parsed = std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| toml::from_str::<PluginFile>(&t).map_err(|e| e.to_string()));
        match parsed {
            Ok(f) => out.extend(f.plugin.into_iter().filter(|p| !p.name.is_empty()).map(|mut p| {
                if p.category.is_empty() {
                    p.category = t(USER_CATEGORY).into();
                }
                p.user = true;
                p
            })),
            Err(e) => errors.push(format!("{name}: {}", e.lines().next().unwrap_or_default())),
        }
    }
    (out, errors)
}

/// File d'esempio scritto la prima volta che si apre la cartella dei plugin.
pub const EXAMPLE_PLUGIN: &str = r#"# Plugin di cyd-deck: ogni [[plugin]] compare nello Store dell'editor.
# Campi: name (obbligatorio), category, description, icon (Material Symbols), label,
# exec (comando), state (exit 0 = attivo), icon_active, label_cmd (output = etichetta).

[[plugin]]
name = "Meteo"
category = "Esempi"
description = "Apre le previsioni di oggi nel terminale"
icon = "partly_cloudy_day"
label = "Meteo"
exec = "foot -H -e curl -s wttr.in/?lang=it"
"#;

pub struct DesktopApp {
    pub name: String,
    pub exec: String,
    pub icon: &'static str,
}

impl DesktopApp {
    pub fn to_plugin(&self) -> Plugin {
        Plugin { name: self.name.clone(), category: APPS_CATEGORY.into(), icon: self.icon.into(), label: self.name.clone(), exec: self.exec.clone(), ..Plugin::default() }
    }

    pub fn to_key(&self) -> Key {
        Key { icon: Some(self.icon.into()), label: Some(self.name.clone()), exec: Some(self.exec.clone()), ..Key::default() }
    }
}

/// Sceglie un'icona Material plausibile dalle categorie del .desktop.
fn icon_for(categories: &str) -> &'static str {
    let has = |c: &str| categories.split(';').any(|x| x == c);
    match () {
        _ if has("WebBrowser") => "public",
        _ if has("TerminalEmulator") => "terminal",
        _ if has("FileManager") => "folder",
        _ if has("TextEditor") || has("IDE") || has("Development") => "code",
        _ if has("Music") || has("Audio") => "music_note",
        _ if has("Video") || has("AudioVideo") => "movie",
        _ if has("Game") => "sports_esports",
        _ if has("Graphics") => "palette",
        _ if has("InstantMessaging") || has("Chat") || has("Network") => "forum",
        _ if has("Office") => "description",
        _ if has("Settings") => "settings",
        _ if has("System") => "settings_applications",
        _ => "apps",
    }
}

/// Elimina i codici di campo (%U, %f, ...) da una riga Exec.
fn clean_exec(exec: &str) -> String {
    exec.split_whitespace()
        .filter(|t| !(t.len() == 2 && t.starts_with('%')) && !t.starts_with("@@"))
        .collect::<Vec<_>>()
        .join(" ")
        .replace("%%", "%")
}

/// App installate (file .desktop visibili), in ordine alfabetico.
pub fn desktop_apps() -> Vec<DesktopApp> {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let dirs = [
        home.join(".local/share/applications"),
        home.join(".local/share/flatpak/exports/share/applications"),
        PathBuf::from("/var/lib/flatpak/exports/share/applications"),
        PathBuf::from("/usr/share/applications"),
    ];
    let mut seen = HashSet::new();
    let mut apps = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let path = e.path();
            let id = e.file_name().to_string_lossy().to_string();
            if !id.ends_with(".desktop") || !seen.insert(id) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let (mut name, mut name_it, mut exec, mut cats) = (None, None, None, String::new());
            let (mut in_entry, mut hidden, mut is_app) = (false, false, false);
            for line in text.lines() {
                let line = line.trim();
                if line.starts_with('[') {
                    in_entry = line == "[Desktop Entry]";
                    continue;
                }
                if !in_entry {
                    continue;
                }
                let Some((k, v)) = line.split_once('=') else { continue };
                match k.trim() {
                    "Name" => name = Some(v.to_string()),
                    "Name[it]" => name_it = Some(v.to_string()),
                    "Exec" => exec = Some(clean_exec(v)),
                    "Categories" => cats = v.to_string(),
                    "Type" => is_app = v == "Application",
                    "NoDisplay" | "Hidden" if v == "true" => hidden = true,
                    _ => {}
                }
            }
            if let (true, false, Some(name), Some(exec)) = (is_app, hidden, name_it.or(name), exec) {
                apps.push(DesktopApp { name, exec, icon: icon_for(&cats) });
            }
        }
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_plugins() {
        let dir = std::env::temp_dir().join(format!("cyd-deck-plugins-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("esempio.toml"), EXAMPLE_PLUGIN).unwrap();
        std::fs::write(dir.join("mio.toml"), "[[plugin]]\nname = \"Ciao\"\nexec = \"notify-send ciao\"\n").unwrap();
        std::fs::write(dir.join("rotto.toml"), "[[plugin]\n").unwrap();
        let (all, errors) = catalog(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].starts_with("rotto.toml"));
        let meteo = all.iter().find(|p| p.name == "Meteo" && p.user).unwrap();
        assert!(meteo.user && meteo.category == "Esempi" && meteo.matches("previsioni"));
        let ciao = all.iter().find(|p| p.name == "Ciao").unwrap();
        assert_eq!(ciao.category, USER_CATEGORY);
        assert_eq!(ciao.to_key().label.as_deref(), Some("Ciao"));
        // Le azioni interne arrivano fino al tasto passando dallo store.
        let off = catalog(Path::new("/nonexistent")).0.into_iter().find(|p| p.action.as_deref() == Some("board-off")).expect("preset Spegni il deck");
        assert_eq!(off.to_key().action.as_deref(), Some("board-off"));
        assert!(all.iter().any(|p| p.category == "Hyprland" && !p.user));
    }
}

#[cfg(test)]
mod icon_tests {
    use super::*;

    /// Ogni icona dei preset esiste nel font Material Symbols (altrimenti il tasto resta vuoto).
    #[test]
    fn preset_icons_exist() {
        let cfg: crate::config::Config = toml::from_str(crate::config::DEFAULT_CONFIG).unwrap();
        let fonts = crate::render::Fonts::load(&cfg.layout.icon_font, &cfg.layout.text_font).unwrap();
        let names: HashSet<String> = crate::icons::ligature_names(fonts.icon_data).into_iter().collect();
        let missing: Vec<&str> = GROUPS
            .iter()
            .flat_map(|g| g.presets)
            .flat_map(|p| [Some(p.icon), p.icon_active])
            .flatten()
            .filter(|i| !i.starts_with('{') && !names.contains(*i))
            .collect();
        assert!(missing.is_empty(), "icone inesistenti: {missing:?}");
    }
}
