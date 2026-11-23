package com.jurisgames.sobres.desktop

import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performTextInput
import com.jurisgames.sobres.core.KtNode
import com.jurisgames.sobres.core.NodeOptions
import com.jurisgames.sobres.core.Platform
import com.jurisgames.sobres.ui.Feedback
import com.jurisgames.sobres.ui.SobresApp
import com.sun.net.httpserver.HttpServer
import org.junit.Rule
import org.junit.Test
import java.io.File
import java.net.InetSocketAddress
import java.nio.file.Files
import java.util.concurrent.CopyOnWriteArrayList

/** Settings > Send feedback: an empty message isn't sent, "See what's sent" is exactly what goes, the diagnostics go
 *  with the tick and never task content, unticked nothing technical goes, and the person is thanked. */
class DesktopFeedbackTest {
    @get:Rule
    val rule = createComposeRule()

    private fun shown(tag: String) = rule.onAllNodes(hasTestTag(tag)).fetchSemanticsNodes().isNotEmpty()
    private fun text(tag: String) = rule.onNodeWithTag(tag).fetchSemanticsNode().config.getOrNull(SemanticsProperties.Text)!!.joinToString { it.text }

    @Test
    fun sendFeedback() {
        val got = CopyOnWriteArrayList<String>()
        val server = HttpServer.create(InetSocketAddress("127.0.0.1", 0), 0)
        server.createContext("/send") { ex ->
            got += ex.requestBody.readBytes().decodeToString()
            val ok = "{\"ok\":true}".toByteArray()
            ex.sendResponseHeaders(200, ok.size.toLong())
            ex.responseBody.use { it.write(ok) }
        }
        server.start()
        Feedback.endpoint = "http://127.0.0.1:${server.address.port}/send"

        val home = Files.createTempDirectory("kt-feedback").toFile()
        val options = NodeOptions(Platform.DESKTOP, listOf("ws://127.0.0.1:9"), false, false, "127.0.0.1:0", "", "")
        val n = KtNode.open(File(home, "identity").path, File(home, "data").path, System.getProperty("kt.worker"), options)
        val list = n.createList(null, "Secret plans")
        n.setEnvelope(list.id, null, "very private task", 100, "m", null, "")
        rule.setContent { SobresApp(n) }
        rule.onNodeWithTag("settingsOpen").performClick()
        rule.waitUntil(5_000) { shown("feedbackOpen") }
        rule.onNodeWithTag("feedbackOpen").performScrollTo().performClick()
        rule.waitUntil(5_000) { shown("feedbackMessage") }

        rule.onNodeWithTag("feedbackSend").performClick()
        rule.waitUntil(5_000) { shown("feedbackError") }
        check(got.isEmpty()) { "an empty message isn't sent" }

        rule.onNodeWithTag("feedbackKind-idea").performClick()
        rule.onNodeWithTag("feedbackMessage").performTextInput("Sync \"stuck\" after\nwaking up")
        rule.onNodeWithTag("feedbackSee").performClick()
        rule.waitUntil(5_000) { shown("feedbackPreview") }
        val preview = text("feedbackPreview")
        check("\"app\": \"sobres\"" in preview && "\"kind\": \"idea\"" in preview && "Nostr relays" in preview) { "the preview shows the report with diagnostics:\n$preview" } // pinned: report format
        check("Secret plans" !in preview && "very private" !in preview) { "no task content in what's sent" }
        rule.onNodeWithTag("feedbackSend").performClick()
        rule.waitUntil(10_000) { shown("feedbackThanks") }
        check(got.size == 1 && got[0] == preview) { "what is sent is exactly what the preview showed" }
        check("Sync \\\"stuck\\\" after\\nwaking up" in got[0]) { "quotes and line breaks are escaped:\n${got[0]}" }

        rule.onNodeWithTag("feedbackOpen").performScrollTo().performClick()
        rule.waitUntil(5_000) { shown("feedbackMessage") }
        rule.onNodeWithTag("feedbackMessage").performTextInput("No details")
        rule.onNodeWithTag("feedbackDetails").performClick()
        rule.onNodeWithTag("feedbackSend").performClick()
        rule.waitUntil(10_000) { got.size == 2 }
        check("\"details\"" !in got[1]) { "unticked, no technical details go" }
        server.stop(0)
        n.close()
    }

    /** pinned: "Send feedback" is in the menu itself, one tap away (Kleider, 2026-11-21: "Make sure users always have an
     *  easy way to give feedback or report bugs on ALL our apps"). */
    @Test
    fun feedbackFromTheMenu() {
        val home = Files.createTempDirectory("kt-feedback-menu").toFile()
        val options = NodeOptions(Platform.DESKTOP, listOf("ws://127.0.0.1:9"), false, false, "127.0.0.1:0", "", "")
        val n = KtNode.open(File(home, "identity").path, File(home, "data").path, System.getProperty("kt.worker"), options)
        n.createList(null, "Home")
        rule.setContent { SobresApp(n) }
        rule.waitUntil(5_000) { shown("feedbackMenu") }
        rule.onNodeWithTag("feedbackMenu").performClick()
        rule.waitUntil(5_000) { shown("feedbackMessage") }
        n.close()
    }
}
