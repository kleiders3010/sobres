package com.jurisgames.tasks.desktop

import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextClearance
import androidx.compose.ui.test.performTextInput
import com.jurisgames.tasks.core.KtNode
import com.jurisgames.tasks.core.NodeOptions
import com.jurisgames.tasks.core.Platform
import com.jurisgames.tasks.ui.TasksApp
import com.jurisgames.tasks.ui.searchTasks
import org.junit.Rule
import org.junit.Test
import java.io.File
import java.nio.file.Files

/**
 * Search, like Microsoft To Do's: one field over every list that finds a word in task titles, steps
 * and notes; capitals and accents don't matter; open tasks before completed ones; tapping a result
 * opens it in its list.
 */
class DesktopSearchTest {
    @get:Rule
    val rule = createComposeRule()

    private fun shown(tag: String) = rule.onAllNodes(hasTestTag(tag), useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty()
    private fun top(tag: String) = rule.onNodeWithTag(tag, useUnmergedTree = true).fetchSemanticsNode().boundsInRoot.top

    private fun node(): KtNode {
        val home = Files.createTempDirectory("kt-search").toFile()
        val options = NodeOptions(Platform.DESKTOP, listOf("ws://127.0.0.1:9"), false, false, "127.0.0.1:0", "", "")
        return KtNode.open(File(home, "identity").path, File(home, "data").path, System.getProperty("kt.worker"), options)
    }

    @Test
    fun findsAcrossListsStepsAndNotes() {
        val n = node()
        val home = n.createList(null, "Home")
        val work = n.createList(null, "Work")
        n.addTask(home.id, "Buy café beans", null)
        val trip = n.addTask(work.id, "Plan trip", null)
        n.addTask(work.id, "book the cafe table", trip) // a step
        val call = n.addTask(work.id, "Call Ana", null)
        n.setNotes(work.id, call, "about the order\nask for the Cafe menu too")
        val old = n.addTask(home.id, "Old cafe receipt", null)
        n.setDone(home.id, old, true)
        n.addTask(home.id, "Dentist", null)

        val found = searchTasks(n, n.lists(), "CAFE")
        check(found.map { it.task.title }.toSet() == setOf("Buy café beans", "book the cafe table", "Call Ana", "Old cafe receipt")) { found.map { it.task.title } }
        check(found.last().task.done) { "completed ones last" }
        check(found.first { it.task.title == "book the cafe table" }.parent == "Plan trip") { "a step says which task it's in" } // pinned: test data, a step shows its parent
        check(found.first { it.task.title == "Call Ana" }.inNotes!!.contains("Cafe menu")) { "the notes line that matched" } // pinned: test data, the matching notes line is shown
        check(searchTasks(n, n.lists(), "cafe menu").map { it.task.title } == listOf("Call Ana")) { "every word must match" }
        check(searchTasks(n, n.lists(), "  ").isEmpty())

        rule.setContent { TasksApp(n, decodeImage = Images::decode, onPickImage = { }) }
        rule.waitUntil(5_000) { shown("searchView") }
        rule.onNodeWithTag("searchView").performClick()
        rule.waitUntil(5_000) { shown("searchField") }
        rule.onNodeWithTag("searchField").performTextInput("cafe")
        rule.waitUntil(5_000) { shown("found-Buy café beans") && shown("found-Call Ana") && shown("found-Old cafe receipt") }
        check(!shown("found-Dentist"))
        check(top("found-Call Ana") < top("completedResults") && top("completedResults") < top("found-Old cafe receipt")) { "completed under their own heading" }
        check(shown("inNotes"))

        rule.onNodeWithTag("searchField").performTextClearance()
        rule.onNodeWithTag("searchField").performTextInput("zzz")
        rule.waitUntil(5_000) { shown("empty") }

        // tapping a result opens it in its list
        rule.onNodeWithTag("searchField").performTextClearance()
        rule.onNodeWithTag("searchField").performTextInput("ana")
        rule.waitUntil(5_000) { shown("found-Call Ana") }
        rule.onNodeWithTag("found-Call Ana", useUnmergedTree = true).performClick()
        rule.waitUntil(5_000) { shown("edit-Call Ana") }
        check(!shown("searchField")) { "back in the list" }
    }
}
