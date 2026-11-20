package com.jurisgames.sobres.desktop

import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.toComposeImageBitmap
import java.awt.image.BufferedImage
import java.io.ByteArrayOutputStream
import java.io.File
import javax.imageio.ImageIO

/** Images on tasks (desktop): a picked file shrunk to the same size as on phones. */
object Images {
    const val MAX_SIDE = 1600

    /** Read, shrink and re-encode as JPEG (transparency goes onto white). */
    fun compress(file: File): ByteArray? = runCatching {
        val src = ImageIO.read(file) ?: return null
        val scale = minOf(1.0, MAX_SIDE.toDouble() / maxOf(src.width, src.height))
        val (w, h) = (src.width * scale).toInt().coerceAtLeast(1) to (src.height * scale).toInt().coerceAtLeast(1)
        val out = BufferedImage(w, h, BufferedImage.TYPE_INT_RGB)
        val g = out.createGraphics()
        g.setRenderingHint(java.awt.RenderingHints.KEY_INTERPOLATION, java.awt.RenderingHints.VALUE_INTERPOLATION_BILINEAR)
        g.color = java.awt.Color.WHITE
        g.fillRect(0, 0, w, h)
        g.drawImage(src, 0, 0, w, h, null)
        g.dispose()
        ByteArrayOutputStream().also { ImageIO.write(out, "jpg", it) }.toByteArray()
    }.getOrNull()

    fun decode(bytes: ByteArray): ImageBitmap? = runCatching { org.jetbrains.skia.Image.makeFromEncoded(bytes).toComposeImageBitmap() }.getOrNull()
}
