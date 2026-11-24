package com.jurisgames.sobres.desktop

import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.SemanticsNode
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.test.ComposeUiTest
import androidx.compose.ui.test.ExperimentalTestApi
import androidx.compose.ui.test.isRoot

/**
 * APP-RULES 196 (from Toma, 2026-11-24: store shots had buttons cut off at a phone's right edge): on a 360 dp wide
 * screen every fixed button is wholly on the screen. Buttons inside a sideways-scrolling row are skipped (an open-ended
 * list, e.g. one chip per list), but the caller names how many such rows the screen has: a new scrolling row of fixed
 * buttons changes the count and fails. positionInWindow + size, not boundsInRoot: that one is clipped by a scrolling
 * row, so a cut-off button looked fine (Toma's first version of this check missed it).
 * Returns the problems found on this screen (empty = fits).
 */
@OptIn(ExperimentalTestApi::class)
fun ComposeUiTest.buttonsFit(widthPx: Int, screen: String, openLists: Int): List<String> {
    waitForIdle()
    val bad = mutableListOf<String>()
    fun all(n: SemanticsNode): List<SemanticsNode> = listOf(n) + n.children.flatMap(::all)
    val nodes = onAllNodes(isRoot()).fetchSemanticsNodes().flatMap(::all)
    fun scrolls(n: SemanticsNode) = n.config.getOrNull(SemanticsProperties.HorizontalScrollAxisRange) != null
    fun inScroll(n: SemanticsNode): Boolean { var p = n.parent; while (p != null) { if (scrolls(p)) return true; p = p.parent }; return false }
    val rows = nodes.count { scrolls(it) && it.children.isNotEmpty() }
    if (rows != openLists) bad += "$screen: $rows sideways-scrolling rows, expected $openLists (fixed buttons must not scroll: APP-RULES 196)"
    val buttons = nodes.filter { it.config.getOrNull(SemanticsProperties.Role) == Role.Button && !inScroll(it) }
    if (buttons.isEmpty()) bad += "$screen: no buttons found"
    fun name(n: SemanticsNode) = n.config.getOrNull(SemanticsProperties.Text)?.joinToString()
        ?: n.config.getOrNull(SemanticsProperties.ContentDescription)?.joinToString() ?: "?"
    val out = buttons.filter { it.positionInWindow.x + it.size.width > widthPx + 0.5f || it.positionInWindow.x < -0.5f }
        .map { name(it) + " at x " + it.positionInWindow.x + " w " + it.size.width }
    if (out.isNotEmpty()) bad += "$screen: buttons past a ${widthPx}px phone's edge: $out"
    return bad
}
