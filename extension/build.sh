#!/bin/sh
# Makes the two loadable folders: dist/chromium (Chrome, Edge, Brave, Chromium, Vivaldi) and dist/firefox. No tools needed.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
for b in chromium firefox; do
  out="$here/dist/$b"
  rm -rf "$out"
  mkdir -p "$out"
  cp -R "$here/src/." "$out/"
  cp "$here/manifest.$b.json" "$out/manifest.json"
  cp "$here/LICENSE-APACHE-2.0" "$here/NOTICE" "$out/"
  echo "wrote $out"
done
