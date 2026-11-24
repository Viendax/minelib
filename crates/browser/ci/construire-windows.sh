#!/usr/bin/env bash
# (v247) Compile le navigateur des écrans (minelib-browser) pour Windows depuis Linux, avec
# MinGW, et prépare ce que la publication met en ligne pour les joueurs :
#
#   minelib-browser_v<N>.exe (+ .sha256)     le navigateur de la version N du jeu ;
#   minelib-cef-<CEF>.tar.gz (+ .sha256)     le runtime Chromium (CEF), commun à toutes les
#                                            versions qui utilisent ce CEF.
#
# Usage : crates/browser/ci/construire-windows.sh <version du jeu> <dossier de sortie>
#
# Il faut : rustup avec la cible x86_64-pc-windows-gnu, mingw-w64 (gcc et strip), curl,
# tar, bzip2, patch. Le CEF Windows est pris chez Spotify (qui publie les binaires de CEF),
# vérifié par son SHA-1 ; cef-dll-sys est pris sur crates.io et corrigé par
# `cef-dll-sys-mingw.patch` (pas de wrapper C++ : la liaison se fait directement sur
# libcef.dll, voir claude/circuit-dev.md). Le Cargo.lock du navigateur est remis tel quel
# à la fin.
set -euo pipefail

N="$1"
OUT="$(mkdir -p "$2" && cd "$2" && pwd)"
ICI="$(cd "$(dirname "$0")" && pwd)"
NAV="$(cd "$ICI/.." && pwd)"
TRAVAIL="${TRAVAIL:-$(mktemp -d)}"
mkdir -p "$TRAVAIL"

CEF_VERSION="150.0.14"
CEF_NOM="cef_binary_150.0.14+g7c1aa68+chromium-150.0.7871.129_windows64_minimal"
CEF_SHA1="63272475eee07ec21cbc8774e2e7e18957115a55"
CRATE_VERSION="150.2.1+150.0.14"

# Le runtime de Chromium que le navigateur charge (les autres langues ne servent pas).
RUNTIME=(
  chrome_100_percent.pak chrome_200_percent.pak chrome_elf.dll d3dcompiler_47.dll
  dxcompiler.dll dxil.dll icudtl.dat libEGL.dll libGLESv2.dll libcef.dll resources.pak
  v8_context_snapshot.bin vk_swiftshader.dll vk_swiftshader_icd.json vulkan-1.dll
  locales/fr.pak locales/en-US.pak
)

# 1. CEF pour Windows, rangé comme l'attend cef-dll-sys (CEF_PATH/<version>/<cible>).
CEF_PATH="$TRAVAIL/cef"
DIR="$CEF_PATH/$CEF_VERSION/cef_windows_x86_64"
if [ ! -f "$DIR/libcef.dll" ]; then
  echo "CEF $CEF_VERSION : téléchargement"
  curl -fsSL --retry 3 -o "$TRAVAIL/cef.tar.bz2" \
    "https://cef-builds.spotifycdn.com/${CEF_NOM//+/%2B}.tar.bz2"
  echo "$CEF_SHA1  $TRAVAIL/cef.tar.bz2" | sha1sum -c -
  tar xjf "$TRAVAIL/cef.tar.bz2" -C "$TRAVAIL"
  rm -f "$TRAVAIL/cef.tar.bz2"
  SRC="$TRAVAIL/$CEF_NOM"
  mkdir -p "$DIR"
  cp -r "$SRC"/Release/. "$DIR"/
  cp -r "$SRC"/Resources/. "$DIR"/
  for f in "$SRC"/*; do
    case "$(basename "$f")" in
      Release | Resources) ;;
      *) cp -r "$f" "$DIR"/ ;;
    esac
  done
  rm -rf "$SRC"
  printf '{\n  "type": "minimal",\n  "name": "%s.tar.bz2",\n  "sha1": "%s"\n}\n' \
    "$CEF_NOM" "$CEF_SHA1" > "$DIR/archive.json"
fi

# 2. cef-dll-sys corrigé pour MinGW.
CRATE="$TRAVAIL/cef-dll-sys-$CRATE_VERSION"
if [ ! -d "$CRATE" ]; then
  echo "cef-dll-sys $CRATE_VERSION : téléchargement et correction"
  curl -fsSL --retry 3 \
    "https://static.crates.io/crates/cef-dll-sys/cef-dll-sys-${CRATE_VERSION//+/%2B}.crate" \
    | tar xz -C "$TRAVAIL"
  patch -d "$CRATE" -p1 < "$ICI/cef-dll-sys-mingw.patch"
fi

# 3. Le navigateur.
cd "$NAV"
cp Cargo.lock "$TRAVAIL/Cargo.lock.depot"
trap 'cp "$TRAVAIL/Cargo.lock.depot" "$NAV/Cargo.lock"' EXIT
CEF_PATH="$CEF_PATH" cargo build --release --target x86_64-pc-windows-gnu \
  --config "patch.crates-io.cef-dll-sys.path=\"$CRATE\""
EXE="minelib-browser_v$N.exe"
x86_64-w64-mingw32-strip --strip-all -o "$OUT/$EXE" \
  target/x86_64-pc-windows-gnu/release/minelib-browser.exe

# 4. Le runtime de Chromium, en tar (ustar) compressé : le jeu sait l'ouvrir.
CEF_TGZ="minelib-cef-$CEF_VERSION.tar.gz"
tar --format=ustar --owner=0 --group=0 --numeric-owner --mtime='2026-01-01' \
  -czf "$OUT/$CEF_TGZ" -C "$DIR" "${RUNTIME[@]}"

cd "$OUT"
for f in "$EXE" "$CEF_TGZ"; do
  sha256sum "$f" | cut -d' ' -f1 > "$f.sha256"
done
ls -la "$OUT"
