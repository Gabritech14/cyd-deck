#!/bin/sh
# Impacchetta il programma in un'AppImage: un solo file che gira su qualsiasi distro Linux.
# Packs the desktop app into an AppImage.
#   host/packaging/build-appimage.sh <binario cyd-deck> [cartella di uscita]
# Serve appimagetool: se manca lo scarica (release "continuous" del progetto AppImage).
set -eu
BIN=$(readlink -f "$1")
OUT=$(readlink -f "${2:-.}")
HERE=$(dirname "$(readlink -f "$0")")
HOST=$(dirname "$HERE")
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$HOST/Cargo.toml" | head -1)
# APPIMAGE_SUFFIX=-beta per la versione beta (con l'indicatore della batteria).
VERSION="$VERSION${APPIMAGE_SUFFIX:-}"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

APPDIR="$WORK/CYD_Deck.AppDir"
mkdir -p "$APPDIR/usr/bin"
install -m755 "$BIN" "$APPDIR/usr/bin/cyd-deck"
cp "$HOST/assets/cyd-deck.svg" "$APPDIR/cyd-deck.svg"
cp "$HOST/assets/cyd-deck.svg" "$APPDIR/.DirIcon"
cp "$HOST/cyd-deck.desktop" "$APPDIR/cyd-deck.desktop"
cat > "$APPDIR/AppRun" <<'RUN'
#!/bin/sh
# Senza argomenti apre l'editor; "run", "calibrate"... come il programma normale.
HERE=$(dirname "$(readlink -f "$0")")
export PATH="$HERE/usr/bin:$PATH"
[ $# -eq 0 ] && set -- gui
exec "$HERE/usr/bin/cyd-deck" "$@"
RUN
chmod +x "$APPDIR/AppRun"

TOOL=$(command -v appimagetool || true)
if [ -z "$TOOL" ]; then
  TOOL="$WORK/appimagetool"
  curl -fsSL -o "$TOOL" https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
  chmod +x "$TOOL"
fi
# Senza FUSE (per esempio nei server di GitHub) appimagetool si estrae da solo.
APPIMAGE_EXTRACT_AND_RUN=1 ARCH=x86_64 "$TOOL" --no-appstream "$APPDIR" "$OUT/CYD_Deck-$VERSION-x86_64.AppImage"
echo "ok: $OUT/CYD_Deck-$VERSION-x86_64.AppImage"
