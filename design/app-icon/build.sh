#!/bin/sh
# Builds resources/mac/Ass.icns from icon.html: headless Chrome draws every PNG, iconutil packs them.
# Needs Google Chrome in /Applications and macOS's own iconutil. Nothing is downloaded.
#
#   design/app-icon/build.sh              write resources/mac/Ass.icns
#   design/app-icon/build.sh <folder>     also write every PNG of both variants into <folder>,
#                                         as dark-<size>.png (shipped) and light-<size>.png (preview)
set -eu

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
chrome="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
preview=${1:-}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM

# Draws one variant into $work/<variant>/<size>.png.
render() {
  "$chrome" --headless=new --disable-gpu --force-color-profile=srgb --no-first-run \
    --disable-background-networking --disable-component-update --disable-extensions \
    --user-data-dir="$work/chrome-$1" --dump-dom "file://$here/icon.html?variant=$1" \
    > "$work/$1.html" 2> "$work/$1.log"
  mkdir "$work/$1"
  grep -Eo 'png:[0-9]+:[A-Za-z0-9+/=]+' "$work/$1.html" | while IFS=: read -r _ size data; do
    printf '%s' "$data" | base64 -d > "$work/$1/$size.png"
  done
  for size in 16 32 64 128 256 512 1024; do
    [ -s "$work/$1/$size.png" ] || { echo "build.sh: no $size px image for $1" >&2; cat "$work/$1.log" >&2; exit 1; }
  done
}

render dark
set="$work/Ass.iconset"
mkdir "$set"
for size in 16 32 128 256 512; do
  cp "$work/dark/$size.png" "$set/icon_${size}x${size}.png"
  cp "$work/dark/$((size * 2)).png" "$set/icon_${size}x${size}@2x.png"
done
/usr/bin/iconutil -c icns -o "$work/Ass.icns" "$set"
mkdir -p "$repo/resources/mac"
cp "$work/Ass.icns" "$repo/resources/mac/Ass.icns"
echo "wrote $repo/resources/mac/Ass.icns"

if [ -n "$preview" ]; then
  render light
  mkdir -p "$preview"
  for size in 16 32 64 128 256 512 1024; do
    cp "$work/dark/$size.png" "$preview/dark-$size.png"
    cp "$work/light/$size.png" "$preview/light-$size.png"
  done
  echo "wrote both variants to $preview"
fi
