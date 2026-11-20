package com.jurisgames.tasks.desktop

import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
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

/** Notes: short ones inline, long ones as a text attachment, edited the same way. */
class DesktopNotesTest {
    @get:Rule
    val rule = createComposeRule()

    private fun shown(tag: String) = rule.onAllNodes(hasTestTag(tag)).fetchSemanticsNodes().isNotEmpty()

    @Test
    fun shortAndLongNotes() {
        val home = Files.createTempDirectory("kt-notes").toFile()
        val options = NodeOptions(Platform.DESKTOP, listOf("ws://127.0.0.1:9"), false, false, "127.0.0.1:0", "", "")
        val n = KtNode.open(File(home, "identity").path, File(home, "data").path, System.getProperty("kt.worker"), options)
        val list = n.createList(null, "Trip")
        n.addTask(list.id, "Packing", null)
        val essay = n.addTask(list.id, "Essay", null)
        val long = "Everything to remember for the trip. ".repeat(4000) // ~150 KB: an attachment
        n.setNotes(list.id, essay, long)

        rule.setContent { TasksApp(n) }
        rule.onNodeWithTag("list-Trip").performClick()
        rule.waitUntil(5_000) { shown("task-Packing") }
        rule.onNodeWithTag("task-Packing").performClick()
        rule.waitUntil(5_000) { shown("notesText") }
        rule.onNodeWithTag("notesText").performTextInput("passport, charger")
        // Saved as typed, no Save button.
        rule.waitUntil(5_000) { n.notes(list.id, n.tasks(list.id).single { it.title == "Packing" }.id).text == "passport, charger" }

        val t = n.tasks(list.id).single { it.title == "Essay" }
        check(t.hasAttachment && t.notes.length < long.length) { "the list shows a preview" }
        rule.onNodeWithTag("task-Essay").performClick()
        rule.waitUntil(5_000) { shown("edit-Essay") }
        val shownText = rule.onNodeWithTag("notesText").fetchSemanticsNode().config.getOrNull(SemanticsProperties.EditableText)?.text
        check(shownText == long) { "the whole attachment opens (${shownText?.length} of ${long.length})" }
        check(!shown("notesPending"))
        n.close()
    }
}
