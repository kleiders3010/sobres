#!/bin/bash
# Build the Rust core for the Android app: libkt_ffi.so (UniFFI bridge) and the import
# sandbox worker (shipped as libkt_import.so so Android lets the app execute it), for real
# devices (arm64-v8a) and the emulator (x86_64). Then regenerate the Kotlin bindings.
set -euo pipefail
cd "$(dirname "$0")/.."
export ANDROID_NDK_HOME="$(cygpath -w ~/AppData/Local/Android/Sdk/ndk/29.0.14206865 2>/dev/null || echo ~/Android/Sdk/ndk/29.0.14206865)"
OUT=app/android/src/main/jniLibs
cargo ndk -t arm64-v8a -t x86_64 -P 26 -o "$OUT" build --release -p kt-ffi  # phone build: no relay server (D51)
for abi in arm64-v8a:aarch64-linux-android x86_64:x86_64-linux-android; do
  a=${abi%%:*}; t=${abi##*:}
  cargo ndk -t "$a" -P 26 build --release -p kt-engine --bin kt-import-worker
  cp "target/$t/release/kt-import-worker" "$OUT/$a/libkt_import.so"
done
cargo build -p kt-ffi
cargo run -q -p kt-ffi --features bindgen --bin uniffi-bindgen -- generate --library target/debug/kt_ffi.dll --language kotlin --out-dir target/uniffi-kotlin --no-format
cp target/uniffi-kotlin/com/jurisgames/sobres/core/kt_ffi.kt app/shared/src/com/jurisgames/sobres/core/kt_ffi.kt
# only our two libraries ship: cargo ndk also copies dependencies' own dylibs (libiroh*.so before), which
# nothing loads (everything links into libkt_ffi.so); keep-list, not a name pattern, so a new one is caught too
find "$OUT" -name '*.so' ! -name libkt_ffi.so ! -name libkt_import.so -delete
for abi in arm64-v8a x86_64; do
  got=$(ls "$OUT/$abi" | sort | tr '\n' ' ')
  [ "$got" = "libkt_ffi.so libkt_import.so " ] || { echo "unexpected libraries in $OUT/$abi: $got"; exit 1; }
done
ls -la "$OUT"/*/
