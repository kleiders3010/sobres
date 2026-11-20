package com.jurisgames.tasks

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File

/** Device keys in OS secure storage (DESIGN §9): the identity is kept sealed by the Keystore. */
@RunWith(AndroidJUnit4::class)
class KeystoreTest {
    @Test
    fun identityIsSealedByTheKeystore() {
        val ctx = InstrumentationRegistry.getInstrumentation().targetContext
        val node = (ctx.applicationContext as TasksApplication).node
        check(node.deviceKey().isNotEmpty())
        val dir = File(ctx.noBackupFilesDir, "identity")
        check(File(dir, "device.sealed").exists()) { "sealed identity: ${dir.list()?.toList()}" }
        check(!File(dir, "device.key").exists()) { "no plain copy" }
        val secret = "device secret".toByteArray()
        val sealed = KeystoreVault.seal(secret)
        check(!String(sealed, Charsets.ISO_8859_1).contains("device secret"))
        check(KeystoreVault.open(sealed).contentEquals(secret))
        sealed[sealed.size - 1] = (sealed[sealed.size - 1].toInt() xor 1).toByte()
        check(runCatching { KeystoreVault.open(sealed) }.isFailure) { "tampering is caught" }
    }
}
