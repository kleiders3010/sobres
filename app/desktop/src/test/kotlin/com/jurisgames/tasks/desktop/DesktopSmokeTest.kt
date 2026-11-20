package com.jurisgames.tasks.desktop

import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import com.jurisgames.tasks.core.KtNode
import com.jurisgames.tasks.core.NodeOptions
import com.jurisgames.tasks.core.Platform
import com.jurisgames.tasks.ui.TasksApp
import org.junit.Rule
import org.junit.Test
import java.io.File
import java.nio.file.Files

/** Headless desktop UI test (offscreen): Compose UI → JNA/UniFFI → the Rust library. */
class DesktopSmokeTest {
    @get:Rule
    val rule = createComposeRule()

    @Test
    fun createAListAndATask() {
        val home = Files.createTempDirectory("kt-desktop").toFile()
        val worker = System.getProperty("kt.worker")
        // No network: a dead relay and no P2P (nothing leaves this machine).
        val node = KtNode.open(File(home, "identity").path, File(home, "data").path, worker, NodeOptions(Platform.DESKTOP, listOf("ws://127.0.0.1:9"), false, false, "127.0.0.1:0", "", ""))
        rule.setContent { TasksApp(node) }
        // First launch: ready to type into; the first task makes the list.
        rule.waitUntil(5_000) { rule.onAllNodes(hasTestTag("firstLink")).fetchSemanticsNodes().isNotEmpty() }
        check(node.isPristine()) { "nothing made yet, so a backup can still be restored" }
        rule.onNodeWithTag("newtask").performTextInput("call the bank")
        rule.onNodeWithTag("addtask").performClick()
        rule.waitUntil(5_000) { rule.onAllNodes(hasTestTag("task-call the bank")).fetchSemanticsNodes().isNotEmpty() }
        check(node.lists().size == 1)
        rule.onNodeWithTag("newList").performClick()
        rule.onNodeWithTag("newListName").performTextInput("Groceries")
        rule.onNodeWithTag("newListGo").performClick()
        rule.waitUntil(5_000) { rule.onAllNodes(hasTestTag("list-Groceries")).fetchSemanticsNodes().isNotEmpty() }
        // The new list is the one shown.
        rule.onNodeWithTag("newtask").performTextInput("milk")
        rule.onNodeWithTag("addtask").performClick()
        rule.waitUntil(5_000) { rule.onAllNodes(hasTestTag("task-milk")).fetchSemanticsNodes().isNotEmpty() }
        rule.onNodeWithTag("done-milk").performClick()
        check(node.tasks(node.lists().first { it.name == "Groceries" }.id).single().done) { "done state reached the engine" }
        // Sharing a personal list makes it a shared one (the person never sees "group").
        rule.onNodeWithTag("share").performClick()
        rule.waitUntil(5_000) { rule.onAllNodes(hasTestTag("words")).fetchSemanticsNodes().isNotEmpty() }
        check(node.groups().any { !it.personal && it.name == "Groceries" }) { node.groups().toString() }
        check(node.lists().count { it.name == "Groceries" } == 1)
        // An invite from the desktop app: 8 words.
        val family = node.createGroup("Family")
        val invite = node.invite(family.id, 30u)
        check(invite.words.size == 8)
        check(invite.link.startsWith("https://apps.jurisgames.com/join#" + invite.words.joinToString("-"))) { invite.link }
        check(com.jurisgames.tasks.core.qrMatrix(invite.link).size >= 21u)
    }
}
