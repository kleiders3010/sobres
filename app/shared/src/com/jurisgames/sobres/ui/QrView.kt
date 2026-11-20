package com.jurisgames.sobres.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import com.jurisgames.sobres.core.qrMatrix

/** A share link as a QR code (black on white with the standard quiet zone, any theme). */
@Composable
fun QrView(text: String, modifier: Modifier = Modifier) {
    val m = remember(text) { runCatching { qrMatrix(text) }.getOrNull() } ?: return
    Canvas(modifier.size(220.dp).testTag("qr")) {
        val quiet = 4
        val n = m.size.toInt()
        val cell = size.minDimension / (n + 2 * quiet)
        drawRect(Color.White, Offset.Zero, Size(size.minDimension, size.minDimension))
        for (y in 0 until n) for (x in 0 until n) {
            if (m.dark[y * n + x]) drawRect(Color.Black, Offset((x + quiet) * cell, (y + quiet) * cell), Size(cell, cell))
        }
    }
}
