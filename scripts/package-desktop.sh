#!/usr/bin/env bash
# Package the desktop app (Windows or Linux): release Rust core with the relay server
# (D51), the import worker, then Compose's distributable (the app with its own runtime).
# Output: app/desktop/build/compose/binaries/main/app/Sobres/
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build -q --release -p kt-ffi --features desktop
cargo build -q --release -p kt-engine --bin kt-import-worker
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) os=windows; lib=kt_ffi.dll; worker=kt-import-worker.exe ;;
  *) os=linux-x64; lib=libkt_ffi.so; worker=kt-import-worker ;;
esac
out=app/desktop/build/native/$os
mkdir -p "$out"
cp "target/release/$lib" "target/release/$worker" "$out/"
(cd app && ./gradlew -q :desktop:createDistributable)
ls app/desktop/build/compose/binaries/main/app/Sobres
