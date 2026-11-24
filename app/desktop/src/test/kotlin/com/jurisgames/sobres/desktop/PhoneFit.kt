package com.jurisgames.sobres.desktop

import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.text.TextLayoutResult
import androidx.compose.ui.unit.dp
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
    // 2026-11-24 (Papeles review U-P4/U-P3, swept to every app): the Papeles Spanish store picture showed "Más grande" as "M / ás / gr / an / de" and the
    // reader's name squeezed to nothing, and this check passed both (nothing was past the edge). A text laid out on
    // more lines than it has words had a word broken; a screen's title narrower than 80 dp can't be read.
    for (n in nodes) {
        if (inScroll(n)) continue
        val get = n.config.getOrNull(SemanticsActions.GetTextLayoutResult)?.action ?: continue
        val res = mutableListOf<TextLayoutResult>()
        if (get(res) != true) continue
        val r = res.firstOrNull() ?: continue
        val text = r.layoutInput.text.text
        val words = text.split(Regex("\\s+")).count { it.isNotEmpty() }
        if (words > 0 && r.lineCount > words) bad += "$screen: \"$text\" is broken inside a word (${r.lineCount} lines for $words words)"
    }
    // 2026-11-24 (from QR's emulator check): a title whose text is cut ("Make a co…") or wraps is squeezed too, even
    // when it is wider than 80 dp; under 80 dp stays the rule for a title that can't be measured.
    val minTitle = with(density) { 80.dp.toPx() }
    for (n in nodes.filter { it.config.getOrNull(SemanticsProperties.TestTag) == "title" }) {
        val res = mutableListOf<TextLayoutResult>()
        val r = if (n.config.getOrNull(SemanticsActions.GetTextLayoutResult)?.action?.invoke(res) == true) res.firstOrNull() else null
        val cut = r != null && (r.lineCount > 1 || r.multiParagraph.didExceedMaxLines || r.multiParagraph.intrinsics.maxIntrinsicWidth > r.size.width + 2f)
        if (cut || n.size.width < minTitle) bad += "$screen: the title is squeezed to ${n.size.width}px (cut, wrapped or under 80 dp): too many buttons in the top row"
    }
    return bad
}
