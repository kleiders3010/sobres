package com.jurisgames.sobres.desktop

import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import com.jurisgames.sobres.core.KtNode
import com.jurisgames.sobres.core.NodeOptions
import com.jurisgames.sobres.core.Platform
import com.jurisgames.sobres.ui.SobresApp
import org.junit.After
import org.junit.Rule
import org.junit.Test
import java.io.File
import java.nio.file.Files

/**
 * Device linking through the UI (D6): a computer with a list of its own links to a phone's
 * account. Two nodes in this process talk through a local loopback relay (nothing leaves
 * this machine).
 */
class DesktopLinkTest {
    @get:Rule
    val rule = createComposeRule()
    private val port = 17600 + (ProcessHandle.current().pid() % 300).toInt()
    private val relay: Process = ProcessBuilder(System.getProperty("kt.cli"), "relay", "--port", port.toString()).redirectErrorStream(true).start().also {
        it.inputStream.bufferedReader().readLine() // "test relay listening on ..."
    }

    @After
    fun stopRelay() {
        relay.destroyForcibly()
    }

    private fun node(tag: String, deviceName: String): KtNode {
        val home = Files.createTempDirectory("kt-$tag").toFile()
        val options = NodeOptions(Platform.DESKTOP, listOf("ws://127.0.0.1:$port"), false, false, "127.0.0.1:0", deviceName, "")
        return KtNode.open(File(home, "identity").path, File(home, "data").path, System.getProperty("kt.worker"), options)
    }

    @Test
    fun linkThisComputerToAPhone() {
        val phone = node("phone", "Pixel 7")
        phone.setDisplayName("Alex")
        phone.createList(null, "Groceries")
        val desk = node("desk", "Alex's PC")
        desk.createList(null, "Desk notes")
        val words = phone.linkCode().link // pasted as a link (the words work the same)
        check(words.startsWith("https://apps.jurisgames.com/sobres/link#")) { words }
        phone.syncNow() // the link record goes out

        rule.setContent { SobresApp(desk) }
        rule.onNodeWithTag("linkThis").performClick()
        rule.onNodeWithTag("linkThisWords").performTextInput(words)
        rule.onNodeWithTag("linkFind").performClick()
        rule.waitUntil(30_000) { rule.onAllNodes(hasTestTag("linkPreview")).fetchSemanticsNodes().isNotEmpty() }
        rule.onNode(hasTestTag("linkPreview") and hasText("Alex's lists: Pixel 7 · 1 list (Groceries)", substring = true)).assertExists()
        rule.onNodeWithTag("linkGo").performClick()
        rule.waitUntil(30_000) { rule.onAllNodes(hasTestTag("linkResult") and hasText("Linking", substring = true)).fetchSemanticsNodes().isNotEmpty() }
        check(desk.isLinking())

        // Both devices sync as they would in the background; the computer is let in.
        val deadline = System.currentTimeMillis() + 180_000
        while (desk.lists().map { it.name }.sorted() != listOf("Desk notes", "Groceries") || phone.lists().size < 2) {
            check(System.currentTimeMillis() < deadline) { "not linked: desk=${desk.lists().map { it.name }} phone=${phone.lists().map { it.name }}" }
            phone.syncNow()
            desk.syncNow()
        }
        check(!desk.isLinking())
        check(desk.displayName() == "Alex")
        check(phone.recentLinks().single().name == "Alex's PC")
        rule.waitUntil(10_000) { rule.onAllNodes(hasTestTag("list-Groceries")).fetchSemanticsNodes().isNotEmpty() }
        rule.onNode(hasTestTag("linkDevice") and hasText("Your devices (2)", substring = true)).assertExists()
        check(desk.devices().map { it.name }.toSet() == setOf("Pixel 7", "Alex's PC"))
        phone.close()
        desk.close()
    }
}
