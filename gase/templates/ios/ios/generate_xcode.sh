#!/bin/sh
# Generate an Xcode project for "@TITLE@" (run on macOS).
set -e
HERE="$(cd "$(dirname "$0")" && pwd)"
SDL="${SDL2_SOURCE_DIR:-$HERE/../third_party/SDL2}"
if [ ! -f "$SDL/CMakeLists.txt" ]; then
    echo "SDL2 sources not found in $SDL: run scripts/fetch_sdl2.sh first" >&2
    exit 1
fi
cmake -S "$HERE/.." -B "$HERE/build" -G Xcode \
    -DCMAKE_SYSTEM_NAME=iOS \
    -DCMAKE_OSX_DEPLOYMENT_TARGET=13.0 \
    -DGASE_SDL2_SOURCE_DIR="$SDL"
echo "Open $HERE/build/@ID@.xcodeproj, pick your team in Signing & Capabilities, then Run."
