#!/usr/bin/env bash
# Android backup round trip on a device/emulator (DESIGN §11), with the local backup
# transport (no Google account involved):
#   fresh install → make a list → daily backup file written → bmgr backup →
#   uninstall → reinstall (Android restores the sealed file) → first launch restores it.
# Usage: scripts/android-backup-test.sh SERIAL
set -euo pipefail
S=${1:?serial}
ADB=~/AppData/Local/Android/Sdk/platform-tools/adb.exe
PKG=com.jurisgames.sobres
APK=app/android/build/outputs/apk/play/debug/android-play-debug.apk
TEST_APK=app/android/build/outputs/apk/androidTest/play/debug/android-play-debug-androidTest.apk
a() { MSYS_NO_PATHCONV=1 "$ADB" -s "$S" "$@"; }
run() { a shell am instrument -w -e backup "$1" -e class $PKG.BackupFlowTest $PKG.test/androidx.test.runner.AndroidJUnitRunner | tr -d '\r' | grep -E "^OK|FAIL|Error" ; }

a shell bmgr enable true >/dev/null
a shell bmgr transport com.android.localtransport/.LocalTransport >/dev/null
a uninstall $PKG >/dev/null 2>&1 || true
a shell bmgr wipe com.android.localtransport/.LocalTransport $PKG >/dev/null # start clean
a install -r "$APK" >/dev/null
a install -r "$TEST_APK" >/dev/null
a shell input keyevent KEYCODE_WAKEUP
echo "== make"; run make
a shell run-as $PKG ls -la files/backup
# Launch normally (no force-stop): Android 15+ leaves stopped apps out of backups.
a shell am start -W -n $PKG/.MainActivity >/dev/null
a shell input keyevent KEYCODE_HOME
echo "== backup"; a shell bmgr backupnow $PKG | tail -2
a uninstall $PKG >/dev/null
a install "$APK" >/dev/null # restored at install: only the sealed file
a install -r "$TEST_APK" >/dev/null
a shell run-as $PKG ls files
echo "== check"; run check
