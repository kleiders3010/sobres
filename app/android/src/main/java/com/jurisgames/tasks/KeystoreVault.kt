package com.jurisgames.tasks

import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import com.jurisgames.tasks.core.KeyVault
import com.jurisgames.tasks.core.KtException
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Device keys in OS secure storage (DESIGN §9): the identity file is sealed with an AES key
 * inside the Android Keystore (hardware-backed where the phone has it), which never leaves
 * it. No user authentication, so background sync works while the phone is locked.
 */
object KeystoreVault : KeyVault {
    private const val ALIAS = "kt-identity"

    private fun key(): SecretKey {
        val ks = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (ks.getKey(ALIAS, null) as? SecretKey)?.let { return it }
        val gen = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
        gen.init(
            KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build(),
        )
        return gen.generateKey()
    }

    override fun seal(plain: ByteArray): ByteArray = guard {
        val c = Cipher.getInstance("AES/GCM/NoPadding").apply { init(Cipher.ENCRYPT_MODE, key()) }
        byteArrayOf(c.iv.size.toByte()) + c.iv + c.doFinal(plain)
    }

    override fun open(sealed: ByteArray): ByteArray = guard {
        val n = sealed[0].toInt()
        val c = Cipher.getInstance("AES/GCM/NoPadding").apply {
            init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(128, sealed, 1, n))
        }
        c.doFinal(sealed, 1 + n, sealed.size - 1 - n)
    }

    private fun guard(f: () -> ByteArray): ByteArray = try {
        f()
    } catch (e: Exception) {
        throw KtException.Failed(e.toString())
    }
}
