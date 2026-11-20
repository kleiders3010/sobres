# JNA finds native methods, structures and callbacks by reflection.
-keep class com.sun.jna.** { *; }
-keep class * extends com.sun.jna.** { *; }
-keep class * implements com.sun.jna.** { *; }
-dontwarn java.awt.**
# UniFFI bindings: JNA maps these names to the Rust library's symbols.
-keep class com.jurisgames.tasks.core.** { *; }
# ML Kit (Google's code scanner, play flavor) finds its components by reflection from
# manifest entries; shrinking them broke GmsBarcodeScanning.getClient (a crash on a real
# phone, 2026-09-25).
-keep class com.google.mlkit.** { *; }
-keep class com.google.android.gms.internal.mlkit_code_scanner.** { *; }
-keep class * implements com.google.firebase.components.ComponentRegistrar { *; }
-keep class com.google.firebase.components.** { *; }
