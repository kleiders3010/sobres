package com.jurisgames.sobres.desktop

import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import com.jurisgames.sobres.core.KtNode
import com.jurisgames.sobres.core.NodeOptions
import com.jurisgames.sobres.core.Platform
import com.jurisgames.sobres.ui.SobresApp
import org.junit.Rule
import org.junit.Test
import java.io.File
import java.nio.file.Files

/** The hidden diagnostics screen: reached from Settings, shows every layer and the sync log, no task content. */
class DesktopDiagnosticsTest {
    @get:Rule
    val rule = createComposeRule()

    private fun shown(tag: String) = rule.onAllNodes(hasTestTag(tag)).fetchSemanticsNodes().isNotEmpty()

    @Test
    fun hiddenDiagnostics() {
        val home = Files.createTempDirectory("kt-diag").toFile()
        val options = NodeOptions(Platform.DESKTOP, listOf("ws://127.0.0.1:9"), false, false, "127.0.0.1:0", "", "")
        val n = KtNode.open(File(home, "identity").path, File(home, "data").path, System.getProperty("kt.worker"), options)
        val list = n.createList(null, "Secret plans")
        n.setEnvelope(list.id, null, "very private task", 100, "m", null, "")
        n.syncNow()
        rule.setContent { SobresApp(n) }
        rule.onNodeWithTag("settingsOpen").performClick()
        rule.waitUntil(5_000) { shown("version") }
        rule.onNodeWithTag("version").performScrollTo() // the settings scroll: a tap off-screen would close the dialog
        repeat(6) { rule.onNodeWithTag("version").performClick() }
        check(!shown("diagnostics")) { "hidden until the seventh tap" }
        rule.onNodeWithTag("version").performClick()
        rule.waitUntil(5_000) { shown("diagnostics") }
        val text = rule.onNodeWithTag("diagnostics").fetchSemanticsNode().config.getOrNull(SemanticsProperties.Text)!!.joinToString { it.text }
        for (part in listOf("Layers", "Nostr relays:", "Direct connections:", "Storage helpers", "DHT:", "Relaying for others:", "Your own:", "Sync log", "sent ")) {
            check(part in text) { "missing $part in:\n$text" }
        }
        check("Secret plans" !in text && "very private" !in text) { "no task content in diagnostics" }
        n.close()
    }
}
