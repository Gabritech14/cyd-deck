<h1 align="center">CYD Deck</h1>

<p align="center"><b>Your deck, your rules.</b><br>
Un macropad touch open source stile Stream Deck per la scheda Freenove ESP32-S3 2,8".</p>

<p align="center">
  <a href="https://gabritech14.github.io/cyd-deck/"><img alt="Flasha dal browser" src="https://img.shields.io/badge/%E2%9A%A1%20Flasha%20dal%20browser-ffb878?style=for-the-badge&labelColor=4c2700"></a>
</p>

<p align="center">
  <a href="LICENSE"><img alt="Licenza MIT" src="https://img.shields.io/badge/licenza-MIT-ffb878"></a>
  <a href="https://github.com/Gabritech14/cyd-deck/releases/latest"><img alt="Ultima versione" src="https://img.shields.io/github/v/release/Gabritech14/cyd-deck?color=ffb878"></a>
  <img alt="Linux" src="https://img.shields.io/badge/desktop-Linux-ffb878">
  <img alt="Rust" src="https://img.shields.io/badge/fatto%20in-Rust-ffb878">
</p>

<p align="center">🇬🇧 <a href="README.md">Read it in English</a></p>

**Per partire in fretta:** [flasha la scheda dal browser](https://gabritech14.github.io/cyd-deck/) → [installa il programma per il PC](#per-partire) → apri l'editor e segui la breve guida.

Volevo uno Stream Deck ma non volevo pagarlo, così me lo sono costruito con una scheda ESP32 da una ventina
di euro con uno schermo touch da 2,8". Alla fine è diventato qualcosa di più di una griglia di tasti: pagine
che cambiano da sole quando apri un'app, un mixer con il volume di ogni app, la scheda del brano in
riproduzione, l'orologio, Home Assistant e la stampante Bambu Lab, e va anche a batteria via Wi-Fi se lo vuoi
dall'altra parte della scrivania.

La scheda è una Freenove **FNK0104** (ESP32-S3, 240×320, touch capacitivo). Tutto viene disegnato sul PC e
la scheda mostra solo i pixel che riceve e rimanda i tocchi: per cambiare un tasto non serve mai riflashare.

```
firmware/   firmware Rust no_std per la scheda (esp-hal + embassy)
host/       la parte sul PC: programma in background che comanda il deck, più l'editor grafico
```

**A che punto è:** lo uso tutti i giorni su Linux (Hyprland + la shell Caelestia, da cui prende i colori).
Windows e macOS per ora non ci sono. Se li vuoi apri una issue e raccontami per cosa lo useresti:
mi aiuta a capire da dove partire.

## Cosa serve

- scheda Freenove FNK0104 e un cavo USB-C che porti anche i dati
- facoltativa: una LiPo 1S con il connettore della scheda (leggi prima [Sicurezza](#sicurezza))
- il case stampato in 3D (link MakerWorld in arrivo)
- sul PC: Rust, i font **Material Symbols Rounded** e **Rubik**, `fc-match`.
  `wpctl` (PipeWire) e `playerctl` per l'audio, Hyprland per le pagine che seguono l'app in primo piano.
  Caelestia è un bel di più, non è obbligatorio.

## Per partire

**1. Flash del firmware.** Il modo più semplice è il **[web flasher](https://gabritech14.github.io/cyd-deck/)**:
collega la scheda, apri la pagina in Chrome o Edge, clicca installa. Niente da configurare.

Se preferisci compilarlo tu:

```sh
git clone https://github.com/Gabritech14/cyd-deck.git && cd cyd-deck
cargo install espup espflash
espup install
. ~/export-esp.sh
cd firmware && cargo run --release && cd ..
```

Se il flash non parte: tieni premuto BOOT, premi RESET, lascia BOOT.

**2. Installa il programma per il PC** (dalla cartella scaricata):

```sh
git clone https://github.com/Gabritech14/cyd-deck.git && cd cyd-deck   # se non l'hai già fatto
cargo install --path host
cyd-deck            # programma in background che comanda il deck
cyd-deck gui        # l'editor
```

La prima volta che apri l'editor una breve guida ti fa scegliere lingua, colori e pagine di partenza.
Il tuo utente deve poter usare la porta seriale (gruppo `uucp` su Arch, `dialout` su Debian/Ubuntu).
Se i tocchi finiscono nel tasto sbagliato, chiudi `cyd-deck` e lancia una volta `cyd-deck calibrate`.

Per avviarlo con Hyprland aggiungi `exec-once = ~/.cargo/bin/cyd-deck` al tuo `hyprland.conf`.
Lo preferisco a un servizio systemd: le app aperte dal deck ereditano l'ambiente di Hyprland e non si
chiudono se il programma si riavvia.

## L'editor

Quello che vedi nell'editor è esattamente quello che finisce sul deck. Clic su un tasto per modificarlo,
trascinalo per spostarlo, tasto destro per copia/incolla. C'è un selettore con tutte le oltre 4000 icone
Material Symbols, e uno Store con qualche centinaio di azioni pronte (media, Hyprland, sistema, tutte le app
installate).

L'interfaccia è in italiano o in inglese e segue la lingua del sistema; puoi cambiarla in Impostazioni,
insieme ai colori (qualche tema pronto, un colore a piacere, chiaro o scuro).

Qualche cosa da sapere:

- **Macro**: un tasto può fare più passi di fila: aprire app su workspace precisi, aspettare, chiudere
  finestre, cambiare pagina. Comodo per i tasti tipo "preparami la sessione di gioco".
- **Pagina audio**: un cursore per ogni app che sta suonando, più volume generale e microfono, con la scheda
  del brano in alto.
- **Widget nella griglia**: cursori del volume, lettore musicale, orologio o stato della stampante possono
  stare in mezzo ai tasti e occupare più posti.
- **Pagine automatiche**: una pagina può aprirsi da sola quando un'app va in primo piano e tornare indietro
  quando la lasci. Può anche essere nascosta dallo swipe e comparire solo con quell'app.
- **Limiti di Claude Code**: se usi Claude Code, una pagina può mostrare l'uso delle 5 ore e della settimana
  (aggiungi `"statusLine": { "type": "command", "command": "~/.cargo/bin/cyd-deck statusline" }` a `~/.claude/settings.json`).
- **Home Assistant** e **stampanti Bambu Lab** (modalità LAN) compaiono nello Store dopo averli impostati.
- **Plugin**: file `.toml` in `~/.config/cyd-deck/plugins/` per aggiungere allo Store azioni tue.

Tutto sta in `~/.config/cyd-deck/config.toml`, che si ricarica a ogni salvataggio se preferisci scriverlo a mano:

```toml
[[page]]
name = "Media"

[[page.key]]
icon = "volume_up"
icon_active = "volume_off"
label = "Muto"
exec = "wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle"
state = "wpctl get-volume @DEFAULT_AUDIO_SINK@ | grep -q MUTED"   # exit 0 = evidenziato
```

## Wi-Fi e batteria

Imposta la rete in Impostazioni → Wi-Fi della scheda, collega la scheda una volta via USB perché riceva i
dati, poi staccala. Il programma usa l'USB quando c'è e altrimenti trova la scheda in rete.

Vanno solo le reti a 2,4 GHz. Se è lento guarda i dBm sullo schermo di attesa: sotto circa -65 fa fatica.
Il mio era dietro un ripetitore a -70 ed era inutilizzabile, così adesso il portatile crea una piccola rete
solo per il deck:

```sh
nmcli connection add type wifi ifname wlan0 con-name cyd-hotspot autoconnect yes ssid cyd-deck-ap \
  802-11-wireless.mode ap 802-11-wireless.band bg ipv4.method shared \
  wifi-sec.key-mgmt wpa-psk wifi-sec.proto rsn wifi-sec.pairwise ccmp wifi-sec.psk <password>
sudo ufw allow in on wlan0     # solo se usi ufw, altrimenti la scheda non riceve l'indirizzo
```

Con la scheda può parlare un solo programma alla volta. La batteria la ricarica la scheda da sola;
un tasto con `{battery}` come icona o etichetta mostra il livello.

## Note sulla sicurezza

- Password del Wi-Fi, token di Home Assistant e codice della stampante stanno nel portachiavi di sistema, mai nella config.
- PC e scheda si dimostrano a vicenda di conoscere un codice di abbinamento prima di parlarsi (il codice non
  passa mai in rete). Dopo, però, il traffico **non è cifrato**: qualcuno già dentro la tua rete potrebbe in
  teoria mettersi in mezzo e premere i tuoi tasti. Su una rete di casa va bene; se ti preoccupa, usa la rete
  dedicata qui sopra o solo l'USB.
- Sulla scheda la password del Wi-Fi è salvata in chiaro nella memoria flash.
- La chiave della stampante viene memorizzata alla prima connessione; se poi cambia il programma rifiuta di
  collegarsi (cancella `~/.local/share/cyd-deck/printer-keys` se hai sostituito o resettato la stampante).

Hai trovato qualcosa di peggio? Apri una issue o scrivimi in privato, senza pubblicare un exploit.

## Problemi comuni

| Cosa succede | Prova |
|---|---|
| Colori in negativo | `invert = false` in `[device]` |
| Rosso e blu scambiati | `bgr = false` |
| Il tocco prende il tasto sbagliato | `cyd-deck calibrate` |
| Schermo capovolto | `rotation = 3`, poi calibra |
| Fermo su "in attesa della scheda" | `cyd-deck ports` deve mostrare `303a:1001`; controlla il gruppo della seriale |
| Via Wi-Fi non la trova | mentre aspetta la scheda mostra l'IP: mettilo in "Indirizzo fisso" |
| "la scheda ha rifiutato il codice" | il codice è cambiato, collegala una volta via USB |

## Pinout FNK0104

| Funzione | GPIO |
|---|---|
| TFT SCLK / MOSI / MISO | 12 / 11 / 13 |
| TFT CS / DC | 10 / 46 (nessun reset) |
| Retroilluminazione | 45 (alto = acceso) |
| Batteria (partitore 200K/200K) | 9 (ADC1) |
| Touch FT6336 SDA / SCL / RST / INT | 16 / 15 / 18 / 17, indirizzo 0x38 |

## Sicurezza

Le LiPo vanno trattate con rispetto. Usane una con circuito di protezione e controlla la polarità del
connettore prima di collegarla. Non lasciarla in carica senza controllo, non schiacciarla nel case e buttala
se si gonfia. Costruisci e usi il progetto a tuo rischio; il software non ha garanzie (vedi la licenza).

---

# ❤️ Crediti e ringraziamenti

Questo progetto si regge sul lavoro di tantissime persone, regalato a tutti. Grazie a ognuno di voi.

**Hardware**
- [Freenove](https://freenove.com) per la scheda FNK0104, la documentazione e gli esempi
- [Espressif](https://www.espressif.com) per l'ESP32-S3
- la comunità del [Cheap Yellow Display](https://github.com/witnessmenow/ESP32-Cheap-Yellow-Display), da cui viene il "CYD" del nome

**Firmware**
- [esp-rs](https://github.com/esp-rs): `esp-hal`, `esp-radio`, `esp-rtos`, `esp-alloc`, `esp-storage`, `esp-bootloader-esp-idf`, `espflash`, `espup`
- [Embassy](https://embassy.dev) e [smoltcp](https://github.com/smoltcp-rs/smoltcp)
- [embedded-graphics](https://github.com/embedded-graphics/embedded-graphics), `heapless`, `static_cell`, `embedded-io`, `embedded-storage` e i crate [RustCrypto](https://github.com/RustCrypto)

**Programma per il PC**
- [egui / eframe](https://github.com/emilk/egui) di Emil Ernerfeldt e contributori: tutto l'editor, e `egui_kittest` per provarlo
- [ab_glyph](https://github.com/alexheretic/ab-glyph) e [rustybuzz](https://github.com/harfbuzz/rustybuzz) per testo e icone
- [serde](https://serde.rs), [toml](https://github.com/toml-rs/toml), [serde_json](https://github.com/serde-rs/json), [anyhow](https://github.com/dtolnay/anyhow), [ureq](https://github.com/algesten/ureq), [rustls](https://github.com/rustls/rustls), [serialport-rs](https://github.com/serialport/serialport-rs), [secret-service-rs](https://github.com/hwchen/secret-service-rs), [getrandom](https://github.com/rust-random/getrandom)
- [Rust](https://www.rust-lang.org) e la sua comunità
- l'elenco completo delle librerie, con le licenze, è nei file `Cargo.lock`

**Font e icone**
- [Material Symbols](https://fonts.google.com/icons) di Google (Apache 2.0)
- [Rubik](https://fonts.google.com/specimen/Rubik) di Hubert & Fischer (SIL Open Font License)

**Desktop e integrazioni**
- [Caelestia](https://github.com/caelestia-dots/shell) del team caelestia-dots: i colori, il lettore e tanta ispirazione
- [Quickshell](https://quickshell.org), [Hyprland](https://hyprland.org), [PipeWire / WirePlumber](https://pipewire.org), [playerctl](https://github.com/altdesktop/playerctl)
- [Home Assistant](https://www.home-assistant.io)
- chi ha studiato e documentato la modalità LAN delle stampanti Bambu Lab

**Ispirazione e aiuto**
- l'Elgato Stream Deck, da cui è partita l'idea dei macropad con lo schermo
- la comunità di [r/macropad](https://www.reddit.com/r/macropad)
- costruito con l'aiuto di [Claude Code](https://claude.com/claude-code) di Anthropic

Se ho dimenticato qualcuno apri una issue o una PR e ti aggiungo volentieri.

## Licenza

Il codice (firmware e programma per il PC) è [MIT](LICENSE). Il case 3D è su MakerWorld con la sua licenza.

## Marchi

CYD Deck è un progetto amatoriale indipendente, non affiliato né approvato da Elgato/Corsair, Freenove,
Espressif, Anthropic, Bambu Lab o dagli altri progetti citati. "Stream Deck" è un marchio di Corsair/Elgato;
gli altri nomi appartengono ai rispettivi proprietari e sono citati solo per indicare la compatibilità.

---

<p align="center">
  <a href="https://github.com/Gabritech14/cyd-deck">GitHub</a><br>
  <b>With love by gabritech</b> ❤️
</p>
