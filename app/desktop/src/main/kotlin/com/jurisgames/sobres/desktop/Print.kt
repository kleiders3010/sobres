package com.jurisgames.sobres.desktop

import java.awt.print.PageFormat
import java.awt.print.Printable
import javax.swing.JEditorPane

/**
 * Print list on the desktop: the page laid out by Swing's HTML view and sent through the system's print dialog
 * (a printer, or Microsoft Print to PDF). Off the UI thread: the dialog and the printing block until done.
 */
fun printPage(name: String, html: String) {
    Thread({
        runCatching {
            val pane = page(html)
            pane.print(null, null, true, null, null, false)
        }.onFailure { System.err.println("print failed: ${it.message}") }
    }, "print $name").apply { isDaemon = true }.start()
}

/** The page as Swing lays it out (also what the tests draw). */
fun page(html: String): JEditorPane = JEditorPane().apply {
    contentType = "text/html"
    isEditable = false
    // Swing's HTML 3.2 parser stops at a doctype/meta it doesn't know only in old JDKs; strip them anyway.
    text = html.replace(Regex("<!DOCTYPE[^>]*>|<meta[^>]*>"), "")
}

/** Draws page [index] of [html] to an image (tests, review shots): what the printer gets, at 1:1. */
fun pageImage(html: String, index: Int = 0, format: PageFormat = PageFormat()): java.awt.image.BufferedImage? {
    val pane = page(html)
    val printable = pane.getPrintable(null, null)
    val img = java.awt.image.BufferedImage(format.width.toInt(), format.height.toInt(), java.awt.image.BufferedImage.TYPE_INT_RGB)
    val g = img.createGraphics()
    g.color = java.awt.Color.WHITE
    g.fillRect(0, 0, img.width, img.height)
    g.setRenderingHint(java.awt.RenderingHints.KEY_TEXT_ANTIALIASING, java.awt.RenderingHints.VALUE_TEXT_ANTIALIAS_ON)
    val r = printable.print(g, format, index)
    g.dispose()
    return if (r == Printable.PAGE_EXISTS) img else null
}
