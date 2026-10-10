#!/bin/sh
# Download the SDL2 sources next to this project (needed for Android and iOS,
# where SDL is compiled together with the game).
set -e
VERSION=2.30.9
DEST="$(cd "$(dirname "$0")/.." && pwd)/third_party/SDL2"
if [ -f "$DEST/CMakeLists.txt" ]; then echo "SDL2 already in $DEST"; exit 0; fi
mkdir -p "$DEST"
curl -L "https://github.com/libsdl-org/SDL/releases/download/release-$VERSION/SDL2-$VERSION.tar.gz" \
  | tar xz --strip-components=1 -C "$DEST"
echo "SDL2 $VERSION extracted to $DEST"
