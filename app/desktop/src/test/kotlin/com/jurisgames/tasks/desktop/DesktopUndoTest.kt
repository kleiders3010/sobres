package com.jurisgames.tasks.desktop

import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollToNode
import com.jurisgames.tasks.core.KtNode
import com.jurisgames.tasks.core.NodeOptions
import com.jurisgames.tasks.core.Platform
import com.jurisgames.tasks.ui.TasksApp
import org.junit.Rule
import org.junit.Test
import java.io.File
import java.nio.file.Files

/** "Undo changes by [person] since [time]" from the list screen. */
class DesktopUndoTest {
    @get:Rule
    val rule = createComposeRule()

    @Test
    fun undoMyLastHour() {
        val home = Files.createTempDirectory("kt-undo").toFile()
        val options = NodeOptions(Platform.DESKTOP, listOf("ws://127.0.0.1:9"), false, false, "127.0.0.1:0", "", "")
        val node = KtNode.open(File(home, "identity").path, File(home, "data").path, System.getProperty("kt.worker"), options)
        node.setDisplayName("Alex")
        val list = node.createList(null, "Errands")
        val id = node.addTask(list.id, "post office", null)
        node.renameTask(list.id, id, "post office (closed?)")

        rule.setContent { TasksApp(node) }
        rule.waitUntil(5_000) { rule.onAllNodes(hasTestTag("list-Errands")).fetchSemanticsNodes().isNotEmpty() }
        rule.onNodeWithTag("list-Errands").performClick()
        rule.onNodeWithTag("listMenu").performClick()
        rule.onNodeWithTag("undoOpen").performClick()
        rule.onNodeWithTag("undoWho-Alex").performClick()
        rule.onNodeWithTag("undoSince-3600000").performClick()
        rule.onNodeWithTag("undoGo").performClick()
        rule.waitUntil(10_000) { rule.onAllNodes(hasTestTag("undoResult") and hasText("Put back", substring = true)).fetchSemanticsNodes().isNotEmpty() }
        check(node.tasks(list.id).isEmpty()) { "the task made in the last hour is gone again" }
        node.close()
    }
}
