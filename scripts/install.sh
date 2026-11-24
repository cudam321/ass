#!/bin/sh
# Puts the release build where the app is opened from.
#
#   scripts/install.sh            # to target/release/Ass.app
#   scripts/install.sh FOLDER     # to FOLDER/Ass.app
#
# Builds live in target/<profile>/app.noindex/, which Spotlight does not list, and
# the tests run them from there. This is the only step that replaces the app in
# use. It refuses while that app runs, and it swaps the whole bundle with one
# rename, so a copy that fails leaves the old app as it was.
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$ROOT/target/release/app.noindex/Ass.app"
DEST="${1:-$ROOT/target/release}/Ass.app"
STAGE="$(dirname "$DEST")/.ass-install.noindex"

stop() { echo "install: $1" >&2; exit 1; }
inode() { stat -f '%d:%i' "$1" 2>/dev/null; }
sha() { shasum -a 256 "$1" | cut -c1-64; }
# True while any process runs from the app at $DEST. Chromium starts the app
# under other names for the same file (docs/ARCHITECTURE.md, verified fact 25),
# so the main binary is compared by device and inode, not only by path.
running() {
  want="$(inode "$DEST/Contents/MacOS/Ass")"
  [ -n "$want" ] || return 1
  ps -axo comm= | sort -u | while IFS= read -r exe; do
    case "$exe" in "$DEST"/*) echo yes; break ;; esac
    [ "$(inode "$exe")" = "$want" ] && { echo yes; break; }
  done | grep -q yes
}

[ -x "$SRC/Contents/MacOS/Ass" ] || stop "no release build at $SRC. Build it: cargo run --bin bundle -- --release"
[ -d "$(dirname "$DEST")" ] || stop "$(dirname "$DEST") is not a folder"
running && stop "Ass is running from $DEST. Quit it first. Nothing was changed."

rm -rf "$STAGE"
mkdir "$STAGE"
trap 'rm -rf "$STAGE"' EXIT
ditto "$SRC" "$STAGE/Ass.app"
[ "$(sha "$STAGE/Ass.app/Contents/MacOS/Ass")" = "$(sha "$SRC/Contents/MacOS/Ass")" ] || stop "the copy differs from the build. Nothing was changed."

running && stop "Ass was started from $DEST during the copy. Quit it first. Nothing was changed."
[ -e "$DEST" ] && mv "$DEST" "$STAGE/old.app"
mv "$STAGE/Ass.app" "$DEST"

echo "installed: $DEST"
echo "program:   sha256 $(sha "$DEST/Contents/MacOS/Ass")"
