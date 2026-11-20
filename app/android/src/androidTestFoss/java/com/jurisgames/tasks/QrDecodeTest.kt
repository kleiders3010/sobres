package com.jurisgames.tasks

import androidx.test.ext.junit.runners.AndroidJUnit4
import com.jurisgames.tasks.core.qrMatrix
import org.junit.Test
import org.junit.runner.RunWith

/** The foss decoder reads what the app's own QR code shows (a synthetic camera frame). */
@RunWith(AndroidJUnit4::class)
class QrDecodeTest {
    @Test
    fun readsOurOwnQr() {
        val link = "https://apps.jurisgames.com/join#" + List(8) { "word" }.joinToString("-")
        val m = qrMatrix(link)
        val n = m.size.toInt()
        val scale = 8
        val quiet = 4
        val side = (n + 2 * quiet) * scale
        val y = ByteArray(side * side) { 0xFF.toByte() }
        for (row in 0 until n) for (col in 0 until n) if (m.dark[row * n + col]) {
            for (dy in 0 until scale) for (dx in 0 until scale) {
                y[((row + quiet) * scale + dy) * side + (col + quiet) * scale + dx] = 0
            }
        }
        check(QrDecode.fromLuminance(y, side, side, side) == link)
    }
}
