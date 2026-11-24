#!/bin/sh
# Compila il firmware e aggiorna l'immagine del web flasher (docs/firmware/cyd-deck-fw.bin).
# Builds the firmware and refreshes the web flasher image.
# Dopo: aggiorna "version" in docs/manifest.json e nella pagina, poi commit e push.
set -e
cd "$(dirname "$0")"
. "$HOME/export-esp.sh"
"$HOME/.cargo/bin/cargo" build --release
"$HOME/.cargo/bin/espflash" save-image --chip esp32s3 --merge --skip-padding --flash-size 4mb \
  target/xtensa-esp32s3-none-elf/release/cyd-deck-fw ../docs/firmware/cyd-deck-fw.bin
echo "ok: docs/firmware/cyd-deck-fw.bin"
