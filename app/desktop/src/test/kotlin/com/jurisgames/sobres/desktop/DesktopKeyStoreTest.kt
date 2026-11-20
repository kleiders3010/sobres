package com.jurisgames.sobres.desktop

import com.jurisgames.sobres.core.KtNode
import com.jurisgames.sobres.core.NodeOptions
import com.jurisgames.sobres.core.Platform
import org.junit.Test
import java.io.File
import java.nio.file.Files

/** Device keys in OS secure storage (DESIGN §9): on Windows the identity is sealed with DPAPI. */
class DesktopKeyStoreTest {
    @Test
    fun identitySealedOnWindows() {
        if (!System.getProperty("os.name").startsWith("Windows")) return
        val home = Files.createTempDirectory("kt-keystore").toFile()
        val options = NodeOptions(Platform.DESKTOP, listOf("ws://127.0.0.1:9"), false, false, "127.0.0.1:0", "", "")
        val open = { KtNode.open(File(home, "identity").path, File(home, "data").path, System.getProperty("kt.worker"), options) }
        val first = open().use { it.deviceKey() }
        val identity = File(home, "identity")
        check(File(identity, "device.sealed").exists() && !File(identity, "device.key").exists()) { identity.list()!!.toList().toString() }
        check(open().use { it.deviceKey() } == first) { "same device after reopening" }
    }
}
