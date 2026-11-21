# CYD Deck

🇮🇹 [Leggilo in italiano](README.it.md)

I wanted a Stream Deck but didn't want to pay for one, so I built my own out of a ~€20 ESP32 board with a
2.8" touch screen. It turned into a bit more than a grid of buttons: pages that switch on their own when you
focus an app, a per-app volume mixer, a now-playing card, a clock, Home Assistant and Bambu Lab printer
controls, and it runs off a battery over Wi-Fi if you want it on the other side of the desk.

The board is a Freenove **FNK0104** (ESP32-S3, 240×320, capacitive touch). Everything is drawn on the PC and
the board just shows the pixels it gets and sends back touches, so you never need to reflash to change a key.

```
firmware/   Rust no_std firmware for the board (esp-hal + embassy)
host/       the desktop side: background app that drives the deck, plus a graphical editor
```

**Where it's at:** I use it every day on Linux (Hyprland + the Caelestia shell, which it takes its colors
from). Windows and macOS aren't supported yet. If you want them, open an issue and tell me what you'd use it
for, it helps me decide what to do first.

## What you need

- Freenove FNK0104 board and a USB-C cable that carries data
- optional: a 1S LiPo with the board's connector (read [Safety](#safety) first)
- the 3D printed case (MakerWorld link coming)
- on the PC: Rust, the **Material Symbols Rounded** and **Rubik** fonts, `fc-match`.
  `wpctl` (PipeWire) and `playerctl` for the audio stuff, Hyprland for pages that follow the focused app.
  Caelestia is a nice extra, not a requirement.

## Getting it running

Flash the firmware:

```sh
cargo install espup espflash
espup install
. ~/export-esp.sh
cd firmware && cargo run --release
```

If flashing doesn't start: hold BOOT, tap RESET, let go of BOOT.

Then the desktop side:

```sh
cargo install --path host
cyd-deck            # background app that drives the deck
cyd-deck gui        # the editor
```

The first time you open the editor a short guide walks you through language, colors and starting pages.
Your user needs access to the serial port (`uucp` group on Arch, `dialout` on Debian/Ubuntu).
If touches land on the wrong key, stop `cyd-deck` and run `cyd-deck calibrate` once.

To start it with Hyprland, add `exec-once = ~/.cargo/bin/cyd-deck` to your `hyprland.conf`.
I prefer that to a systemd service: apps launched from the deck inherit the Hyprland environment and
don't die when the background app restarts.

## The editor

What you see in the editor is exactly what ends up on the deck. Click a key to edit it, drag to move it,
right click to copy/paste. There's a picker with all 4000+ Material Symbols icons, and a Store tab with a few
hundred ready-made actions (media, Hyprland, system stuff, every app installed on your system).

The interface is in English or Italian and follows your system language; you can change it in Settings,
together with the colors (a handful of ready-made themes, any accent color you like, light or dark).

A few things worth knowing:

- **Macros**: one key can run several steps in a row: open apps on specific workspaces, wait, close windows,
  switch page. Handy for "set up my gaming session" kind of buttons.
- **Audio page**: a slider for every app that's playing sound, plus master and mic, and the now-playing card on top.
- **Widgets in the grid**: volume sliders, the music player, a clock or the printer status can sit between
  normal keys and span several slots.
- **Automatic pages**: a page can open by itself when a given app is focused, and go back when you leave.
  It can also be hidden from swiping and only show up for that app.
- **Claude Code limits**: if you use Claude Code, a page can show your 5-hour and weekly usage
  (add `"statusLine": { "type": "command", "command": "~/.cargo/bin/cyd-deck statusline" }` to `~/.claude/settings.json`).
- **Home Assistant** and **Bambu Lab printers** (LAN mode) show up in the Store once configured in Settings.
- **Plugins**: drop `.toml` files in `~/.config/cyd-deck/plugins/` to add your own actions to the Store.

Everything lives in `~/.config/cyd-deck/config.toml`, which reloads on save if you'd rather edit it by hand:

```toml
[[page]]
name = "Media"

[[page.key]]
icon = "volume_up"
icon_active = "volume_off"
label = "Mute"
exec = "wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle"
state = "wpctl get-volume @DEFAULT_AUDIO_SINK@ | grep -q MUTED"   # exit 0 = highlighted
```

## Wi-Fi and battery

Set the network in Settings → Board Wi-Fi, plug the board in once over USB so it gets the settings, then
unplug it. The app uses USB when it's there and finds the board on the network otherwise.

Only 2.4 GHz networks work. If it feels laggy, look at the dBm on the waiting screen: below about -65 it's
going to struggle. Mine sat behind a repeater at -70 and it was unusable, so now my laptop makes a small
hotspot just for the deck:

```sh
nmcli connection add type wifi ifname wlan0 con-name cyd-hotspot autoconnect yes ssid cyd-deck-ap \
  802-11-wireless.mode ap 802-11-wireless.band bg ipv4.method shared \
  wifi-sec.key-mgmt wpa-psk wifi-sec.proto rsn wifi-sec.pairwise ccmp wifi-sec.psk <password>
sudo ufw allow in on wlan0     # only if you use ufw, otherwise the board never gets an address
```

Only one copy of the app can talk to the board at a time. The board charges the battery by itself;
a key with `{battery}` as icon or label shows the level.

## Security notes

- The Wi-Fi password, Home Assistant token and printer code are kept in the system keyring, never in the config.
- PC and board prove to each other that they know a shared pairing code before talking (the code itself never
  goes over the network). After that the traffic is **not encrypted**, so someone already on your network
  could in theory sit in the middle and press your keys. Fine for a home network; if that bothers you,
  use the dedicated hotspot above or just USB.
- The Wi-Fi password is stored unencrypted in the board's flash.
- The printer's key is remembered on the first connection; if it changes later the app refuses to connect
  (delete `~/.local/share/cyd-deck/printer-keys` after replacing or resetting the printer).

Found something worse? Please open an issue or contact me privately rather than posting an exploit.

## Troubleshooting

| What happens | Try |
|---|---|
| Colors inverted | `invert = false` under `[device]` |
| Red and blue swapped | `bgr = false` |
| Touch hits the wrong key | `cyd-deck calibrate` |
| Screen upside down | `rotation = 3`, then calibrate |
| Stuck on "waiting for the board" | `cyd-deck ports` should list `303a:1001`; check the serial group |
| Not found over Wi-Fi | the board shows its IP while waiting, put it in "Fixed address" |
| "the board rejected the pairing code" | the code changed, plug it in over USB once |

## FNK0104 pinout

| Function | GPIO |
|---|---|
| TFT SCLK / MOSI / MISO | 12 / 11 / 13 |
| TFT CS / DC | 10 / 46 (no reset) |
| Backlight | 45 (high = on) |
| Battery (200K/200K divider) | 9 (ADC1) |
| Touch FT6336 SDA / SCL / RST / INT | 16 / 15 / 18 / 17, address 0x38 |

## Safety

LiPo batteries deserve respect. Use one with a protection circuit and check the connector polarity before
plugging it in. Don't leave it charging unattended, don't cram it into the case, and throw it out if it
swells. You build and use this at your own risk; the software comes with no warranty (see the license).

---

# ❤️ Credits and thanks

This project stands on a lot of other people's work, given away for free. Thank you all.

**Hardware**
- [Freenove](https://freenove.com) for the FNK0104 board and its docs and examples
- [Espressif](https://www.espressif.com) for the ESP32-S3
- the [Cheap Yellow Display](https://github.com/witnessmenow/ESP32-Cheap-Yellow-Display) community, where the "CYD" in the name comes from

**Firmware**
- [esp-rs](https://github.com/esp-rs): `esp-hal`, `esp-radio`, `esp-rtos`, `esp-alloc`, `esp-storage`, `esp-bootloader-esp-idf`, `espflash`, `espup`
- [Embassy](https://embassy.dev) and [smoltcp](https://github.com/smoltcp-rs/smoltcp)
- [embedded-graphics](https://github.com/embedded-graphics/embedded-graphics), `heapless`, `static_cell`, `embedded-io`, `embedded-storage`, and the [RustCrypto](https://github.com/RustCrypto) crates

**Desktop app**
- [egui / eframe](https://github.com/emilk/egui) by Emil Ernerfeldt and contributors: the whole editor, and `egui_kittest` for testing it
- [ab_glyph](https://github.com/alexheretic/ab-glyph) and [rustybuzz](https://github.com/harfbuzz/rustybuzz) for text and icons
- [serde](https://serde.rs), [toml](https://github.com/toml-rs/toml), [serde_json](https://github.com/serde-rs/json), [anyhow](https://github.com/dtolnay/anyhow), [ureq](https://github.com/algesten/ureq), [rustls](https://github.com/rustls/rustls), [serialport-rs](https://github.com/serialport/serialport-rs), [secret-service-rs](https://github.com/hwchen/secret-service-rs), [getrandom](https://github.com/rust-random/getrandom)
- [Rust](https://www.rust-lang.org) and its community
- the full list of libraries and their licenses is in the `Cargo.lock` files

**Fonts and icons**
- [Material Symbols](https://fonts.google.com/icons) by Google (Apache 2.0)
- [Rubik](https://fonts.google.com/specimen/Rubik) by Hubert & Fischer (SIL Open Font License)

**Desktop and integrations**
- [Caelestia](https://github.com/caelestia-dots/shell) by the caelestia-dots team: the colors, the player and a lot of the inspiration
- [Quickshell](https://quickshell.org), [Hyprland](https://hyprland.org), [PipeWire / WirePlumber](https://pipewire.org), [playerctl](https://github.com/altdesktop/playerctl)
- [Home Assistant](https://www.home-assistant.io)
- everyone who reverse-engineered and documented the LAN mode of Bambu Lab printers

**Inspiration and help**
- the Elgato Stream Deck, which started the whole idea of a macropad with a screen
- the [r/macropad](https://www.reddit.com/r/macropad) community
- built with the help of [Claude Code](https://claude.com/claude-code) by Anthropic

If I forgot you, open an issue or a PR and I'll gladly add you.

## License

The code (firmware and desktop app) is [MIT](LICENSE). The 3D case is on MakerWorld under its own license.

## Trademarks

CYD Deck is an independent hobby project, not affiliated with or endorsed by Elgato/Corsair, Freenove,
Espressif, Anthropic, Bambu Lab or any other project mentioned here. "Stream Deck" is a trademark of
Corsair/Elgato; other names belong to their owners and are only used to describe compatibility.

---

<p align="center">
  <a href="https://github.com/Gabritech14/cyd-deck">GitHub</a><br>
  <b>With love by gabritech</b> ❤️
</p>
