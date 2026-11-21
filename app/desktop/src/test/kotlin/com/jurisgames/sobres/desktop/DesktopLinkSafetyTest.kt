package com.jurisgames.sobres.desktop

import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.junit4.createComposeRule
import com.jurisgames.sobres.core.KtNode
import com.jurisgames.sobres.core.NodeOptions
import com.jurisgames.sobres.core.Platform
import com.jurisgames.sobres.ui.SobresApp
import org.junit.Rule
import org.junit.Test
import java.io.File
import java.nio.file.Files

/**
 * Safety audit round 4, T5 / T6. pinned: a link fills a screen, it never acts. A join link opens "Join a shared list"
 * with the code filled in and waits for Join; a device link never fills the device-link screen (linking hands over
 * every budget), it asks for the code from the other device's own screen.
 */
class DesktopLinkSafetyTest {
    @get:Rule
    val rule = createComposeRule()

    private fun shown(tag: String) = rule.onAllNodes(hasTestTag(tag)).fetchSemanticsNodes().isNotEmpty()

    private fun node(): KtNode {
        val home = Files.createTempDirectory("kt-linksafety").toFile()
        val options = NodeOptions(Platform.DESKTOP, listOf("ws://127.0.0.1:9"), false, false, "127.0.0.1:0", "", "")
        return KtNode.open(File(home, "identity").path, File(home, "data").path, System.getProperty("kt.worker"), options)
    }

    @Test
    fun joinLinkWaitsForJoin() {
        val n = node()
        val before = n.lists().size
        rule.setContent { SobresApp(n, openedCode = "https://apps.jurisgames.com/sobres/join#acid-acorn-actor-adapt-adult-agent-alarm-album") }
        rule.waitUntil(5_000) { shown("joinGo") }
        rule.waitForIdle()
        Thread.sleep(1_500)
        rule.waitForIdle()
        check(rule.onAllNodes(hasTestTag("joinGo")).fetchSemanticsNodes().isNotEmpty()) { "the join screen stays open, waiting" }
        check(!shown("joinFinding") && !shown("joinResult")) { "nothing was looked up or joined" }
        check(n.lists().size == before)
        n.close()
    }

    @Test
    fun deviceLinkIsNeverPrefilled() {
        val n = node()
        rule.setContent { SobresApp(n, openedCode = "https://apps.jurisgames.com/sobres/link#acid-acorn-actor-adapt-adult-agent-alarm-album") }
        rule.waitUntil(5_000) { shown("linkThisWords") }
        check(shown("linkFromLink")) { "it says why the code must come from the other device" }
        val field = rule.onAllNodes(hasTestTag("linkThisWords")).fetchSemanticsNodes().first()
        val text = field.config.getOrNull(SemanticsProperties.EditableText)?.text ?: ""
        check(text.isEmpty()) { "the code from the link is not filled in: $text" }
        n.close()
    }
}
