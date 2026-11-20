package com.jurisgames.sobres.desktop

import java.io.File
import kotlin.test.Test

/**
 * The Rust core's Android hook is found by its JNI name, which spells out the app's package and
 * class. When the package changed (Tasks -> Sobres) the old name crashed the app at start; only an
 * install on a device showed it. This keeps the two in step.
 */
class PackageNamesTest {
    private fun file(path: String) = listOf(File(path), File("app/$path"), File("../$path"), File("../../$path")).first { it.exists() }

    @Test
    fun jniHookMatchesTheAndroidPackage() {
        val ns = Regex("""namespace\s*=\s*"([^"]+)"""").find(file("android/build.gradle.kts").readText())!!.groupValues[1]
        val rust = file("crates/kt-ffi/src/android.rs").readText()
        val hooks = Regex("""fn (Java_\w+)\(""").findAll(rust).map { it.groupValues[1] }.toList()
        check(hooks.isNotEmpty()) { "no JNI hook found in android.rs" }
        val prefix = "Java_" + ns.replace("_", "_1").replace('.', '_') + "_"
        for (h in hooks) {
            check(h.startsWith(prefix)) { "$h does not match package $ns" }
            val cls = h.removePrefix(prefix).substringBefore('_')
            val src = file("android/src/main/java/" + ns.replace('.', '/') + "/$cls.kt")
            check(src.readText().contains("external fun " + h.substringAfterLast('_'))) { "$cls.kt declares no ${h.substringAfterLast('_')}" }
        }
    }
}
