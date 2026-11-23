import java.util.zip.ZipFile

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
}

android {
    namespace = "com.jurisgames.sobres"
    compileSdk {
        version = release(37) { minorApiLevel = 2 }
    }
    defaultConfig {
        applicationId = "com.jurisgames.sobres"
        minSdk = 26
        targetSdk = 36
        versionCode = 5
        versionName = "1.0.4"
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        manifestPlaceholders["appLabel"] = "@string/app_name"
        // The Rust core is built for these only (scripts/build-android-libs.sh); JNA's
        // libraries for other ABIs would be dead weight.
        ndk { abiFilters += listOf("arm64-v8a", "x86_64") }
    }
    buildFeatures { compose = true }
    // play: Google Play (Block Store, code scanner, ...); foss: no proprietary Play libraries
    // (F-Droid later). The engine has no Play Services dependency either way (DESIGN §12).
    flavorDimensions += "store"
    productFlavors {
        create("play") { dimension = "store" }
        create("foss") { dimension = "store" }
    }
    buildTypes {
        release {
            // R8: what Play users download. Signing happens at release time, outside the repo.
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
        }
        // Battery measurement copies (scripts/battery-measure.sh --pair): the release build
        // under another package and label, installed next to the real app. E.g.
        // ./gradlew :android:assembleFossMeasure -PmeasureId=.m20 -PmeasureLabel="Tasks M20"
        create("measure") {
            initWith(getByName("release"))
            applicationIdSuffix = (project.findProperty("measureId") as String?) ?: ".measure"
            manifestPlaceholders["appLabel"] = (project.findProperty("measureLabel") as String?) ?: "Sobres (measure)"
            signingConfig = signingConfigs.getByName("debug")
            matchingFallbacks += listOf("release")
        }
    }
    // Native libraries stay as files: the import sandbox worker is executed from there.
    packaging { jniLibs { useLegacyPackaging = true } }
    sourceSets["main"].kotlin.directories.add("../shared/src")
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

// Size ceilings (DESIGN §12, E6/E9): a release APK over 20 MB to download or 40 MB installed
// fails the build. Measured per ABI, since a phone gets only its own native libraries:
// download = the APK without other ABIs' libraries; installed = that plus its own libraries
// extracted (legacy packaging).
abstract class SizeCheck : DefaultTask() {
    @get:InputFiles abstract val apkDir: DirectoryProperty
    @get:Internal abstract val loader: Property<com.android.build.api.variant.BuiltArtifactsLoader>

    @TaskAction fun measure() {
        val mb = 1024.0 * 1024.0
        val apks = loader.get().load(apkDir.get())?.elements ?: error("no APK in ${apkDir.get()}")
        for (apk in apks) {
            val f = File(apk.outputFile)
            val packed = mutableMapOf<String, Long>()
            val unpacked = mutableMapOf<String, Long>()
            ZipFile(f).use { z ->
                for (e in z.entries()) {
                    if (!e.name.startsWith("lib/")) continue
                    val abi = e.name.split("/")[1]
                    packed[abi] = (packed[abi] ?: 0L) + e.compressedSize
                    unpacked[abi] = (unpacked[abi] ?: 0L) + e.size
                }
            }
            for (abi in packed.keys.ifEmpty { setOf("any") }) {
                val others = packed.filterKeys { it != abi }.values.sum()
                val download = (f.length() - others) / mb
                val installed = download + (unpacked[abi] ?: 0L) / mb
                logger.lifecycle("size: ${f.name} [$abi] %.1f MB download, %.1f MB installed".format(download, installed))
                if (download > 20.0) throw GradleException("${f.name} [$abi]: %.1f MB download is over the 20 MB ceiling".format(download))
                if (installed > 40.0) throw GradleException("${f.name} [$abi]: %.1f MB installed is over the 40 MB ceiling".format(installed))
            }
        }
    }
}

androidComponents {
    onVariants(selector().withBuildType("release")) { v ->
        val name = v.name.replaceFirstChar { it.uppercase() }
        val check = tasks.register<SizeCheck>("checkSize$name") {
            apkDir.set(v.artifacts.get(com.android.build.api.artifact.SingleArtifact.APK))
            loader.set(v.artifacts.getBuiltArtifactsLoader())
        }
        tasks.matching { it.name == "assemble$name" }.configureEach { finalizedBy(check) }
    }
}

dependencies {
    val bom = platform("androidx.compose:compose-bom:2026.09.00")
    implementation(bom)
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.activity:activity-compose:1.13.0")
    implementation("androidx.work:work-runtime-ktx:2.12.0")
    // Drag to reorder tasks (Apache-2.0; Compose Multiplatform).
    implementation("sh.calvin.reorderable:reorderable:3.1.0")
    // The code scanner brings an old Fragment that breaks registerForActivityResult (lint).
    implementation("androidx.fragment:fragment-ktx:1.9.1")
    // QR scanning: Google's code scanner needs no camera permission; foss scans with the camera.
    add("playImplementation", "com.google.android.gms:play-services-code-scanner:16.1.0")
    add("fossImplementation", "androidx.camera:camera-camera2:1.6.2")
    add("fossImplementation", "androidx.camera:camera-lifecycle:1.6.2")
    add("fossImplementation", "androidx.camera:camera-view:1.6.2")
    add("fossImplementation", "com.google.zxing:core:3.5.4")
    implementation("net.java.dev.jna:jna:5.19.1@aar")
    androidTestImplementation(bom)
    androidTestImplementation("androidx.compose.ui:ui-test-junit4")
    androidTestImplementation("androidx.test.ext:junit:1.3.0")
    androidTestImplementation("androidx.test:runner:1.7.0")
    debugImplementation("androidx.compose.ui:ui-test-manifest")
}
