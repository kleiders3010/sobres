plugins {
    id("org.jetbrains.kotlin.jvm")
    id("org.jetbrains.kotlin.plugin.compose")
    id("org.jetbrains.compose")
}

sourceSets["main"].kotlin.srcDir("../shared/src")

dependencies {
    implementation(compose.desktop.currentOs)
    implementation(compose.material3)
    implementation("net.java.dev.jna:jna:5.19.1")
    // Drag to reorder tasks (Apache-2.0; Compose Multiplatform).
    implementation("sh.calvin.reorderable:reorderable:3.1.0")
    testImplementation(kotlin("test"))
    testImplementation("org.jetbrains.compose.ui:ui-test-junit4:1.12.1")
    testImplementation("junit:junit:4.13.2")
}

compose.desktop {
    application {
        mainClass = "com.jurisgames.sobres.desktop.MainKt"
        nativeDistributions {
            targetFormats(org.jetbrains.compose.desktop.application.dsl.TargetFormat.Msi)
            packageName = "Sobres"
            packageVersion = "1.0.0"
            vendor = "Juris Games"
            description = "Shared envelope budget, no accounts, no servers"
            // JNA needs sun.misc.Unsafe; the rest is Compose's default runtime.
            modules("jdk.unsupported", "java.naming")
            // scripts/package-desktop.sh puts kt_ffi and the import worker here, per OS.
            appResourcesRootDir.set(layout.buildDirectory.dir("native"))
            windows {
                menuGroup = "Sobres"
                perUserInstall = true
                shortcut = true
                upgradeUuid = "74608b51-91e6-4e4a-8ab6-e20062573730"
            }
        }
    }
}

// Tests use the Rust library built by `cargo build -p kt-ffi --features desktop`.
val rustTarget = rootDir.resolve("../target/debug")
tasks.test {
    systemProperty("jna.library.path", rustTarget.absolutePath)
    val exe = if (System.getProperty("os.name").lowercase().contains("win")) ".exe" else ""
    systemProperty("kt.worker", rustTarget.resolve("kt-import-worker$exe").absolutePath)
    systemProperty("kt.cli", rustTarget.resolve("kt$exe").absolutePath) // local test relay
    // ScreenshotTest writes PNGs only when asked: gradlew :desktop:test -Pshots=<folder>
    systemProperty("kt.shots", (project.findProperty("shots") as String?) ?: "")
    // The app follows the device language; tests run in English unless asked: -Plang=es
    systemProperty("kt.lang", (project.findProperty("lang") as String?) ?: "en")
}
