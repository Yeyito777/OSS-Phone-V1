#!/bin/sh
# Saves a picture as a Moon wallpaper: scaled to cover the screen with the
# margin a tilting wallpaper slides in (1160x2420 for the A26's 1080x2340),
# as a PNG in Moon's saved wallpapers (src/a26-shell/src/wallpaper.rs).
#
#   wallpaper-add.sh PICTURE [NAME] [--set]
#
# Any format ImageMagick reads. NAME defaults to the picture's base name;
# --set makes it the wallpaper (a26-shellctl settings set wallpaper
# saved:NAME.png). Install as /opt/a26-shell/bin/moon-wallpaper-add.
set -eu

DIR=/var/lib/a26-shell/wallpapers
SIZE=1160x2420
CTL=${CTL:-/opt/a26-shell/bin/a26-shellctl}

set_it=0
picture=
name=
for arg in "$@"; do
    case "$arg" in
        --set) set_it=1 ;;
        *) if [ -z "$picture" ]; then picture=$arg; else name=$arg; fi ;;
    esac
done
if [ -z "$picture" ] || [ ! -f "$picture" ]; then
    echo "usage: wallpaper-add.sh PICTURE [NAME] [--set]" >&2
    exit 2
fi
if [ -z "$name" ]; then
    name=$(basename "$picture")
    name=${name%.*}
fi
case "$name" in
    */* | .* | "") echo "bad name: $name" >&2; exit 2 ;;
esac

mkdir -p "$DIR"
tmp="$DIR/.$name.png.new"
# The first frame, upright, sRGB, covering SIZE, no alpha; then into place.
magick "$picture[0]" -auto-orient -colorspace sRGB -resize "$SIZE^" \
    -gravity center -extent "$SIZE" -background black -alpha remove \
    -alpha off -strip "PNG24:$tmp"
mv -f "$tmp" "$DIR/$name.png"
echo "$DIR/$name.png"
if [ "$set_it" = 1 ]; then
    "$CTL" settings set wallpaper "saved:$name.png" >/dev/null
fi
