package com.jurisgames.tasks.desktop

import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performImeAction
import androidx.compose.ui.test.performTextInput
import com.jurisgames.tasks.core.KtNode
import com.jurisgames.tasks.core.NodeOptions
import com.jurisgames.tasks.core.Platform
import com.jurisgames.tasks.ui.TasksApp
import com.jurisgames.tasks.ui.fromMillis
import org.junit.Rule
import org.junit.Test
import java.io.File
import java.nio.file.Files
import java.time.DayOfWeek

/**
 * Repeating tasks, like Todoist's: typed "every monday" makes one; ticking it off keeps it in the
 * list, due a week later, with its steps unticked; the Repeat choice in its details turns it off.
 */
class DesktopRepeatTest {
    @get:Rule
    val rule = createComposeRule()

    private fun shown(tag: String) = rule.onAllNodes(hasTestTag(tag), useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty()

    private fun node(): KtNode {
        val home = Files.createTempDirectory("kt-repeat").toFile()
        val options = NodeOptions(Platform.DESKTOP, listOf("ws://127.0.0.1:9"), false, false, "127.0.0.1:0", "", "")
        return KtNode.open(File(home, "identity").path, File(home, "data").path, System.getProperty("kt.worker"), options)
    }

    @Test
    fun everyMondayComesBack() {
        val n = node()
        val list = n.createList(null, "Home")
        rule.setContent { TasksApp(n, decodeImage = Images::decode, onPickImage = { }) }
        rule.onNodeWithTag("list-Home").performClick()
        rule.waitUntil(5_000) { shown("newtask") }

        rule.onNodeWithTag("newtask").performTextInput("bins out every monday")
        rule.waitUntil(5_000) { shown("whenChip") }
        rule.onNodeWithTag("newtask").performImeAction()
        rule.waitUntil(5_000) { n.tasks(list.id).any { it.title == "bins out" } }
        val t = n.tasks(list.id).single()
        check(t.repeat == "1w") { "typed 'every monday' repeats weekly: ${t.repeat}" }
        val due = fromMillis(t.remindMs!!.toLong())
        check(due.dayOfWeek == DayOfWeek.MONDAY) { "on a Monday: $due" }
        val step = n.addTask(list.id, "recycling", t.id)
        n.setDone(list.id, step, true)

        // Tick it off: it stays, a week later, its step unticked
        rule.waitUntil(5_000) { shown("done-bins out") }
        rule.onNodeWithTag("done-bins out", useUnmergedTree = true).performClick()
        rule.waitUntil(5_000) { n.tasks(list.id).first().remindMs != t.remindMs }
        val after = n.tasks(list.id)
        check(!after.first().done) { "a repeating task isn't done, it moves on" }
        check(fromMillis(after.first().remindMs!!.toLong()) == due.plusWeeks(1)) { "a week later" }
        check(after.none { it.done }) { "its steps start over" }
        rule.mainClock.advanceTimeBy(2_000)
        rule.waitForIdle()
        check(shown("task-bins out")) { "still in the list, not folded into Completed" }

        // Details: the Repeat choice shows it, and turns it off
        rule.onNodeWithTag("task-bins out", useUnmergedTree = true).performClick()
        rule.waitUntil(5_000) { shown("repeat-1w") }
        rule.onNodeWithTag("repeat-1w").performClick()
        rule.waitUntil(5_000) { n.tasks(list.id).first().repeat == null }
        // and picking one on a task with no reminder gives it a first date
        val plain = n.addTask(list.id, "stretch", null)
        rule.onNodeWithTag("detailsClose").performClick()
        rule.waitUntil(5_000) { shown("task-stretch") }
        rule.onNodeWithTag("task-stretch", useUnmergedTree = true).performClick()
        rule.waitUntil(5_000) { shown("repeat-1d") }
        rule.onNodeWithTag("repeat-1d").performClick()
        rule.waitUntil(5_000) { n.tasks(list.id).first { it.id == plain }.let { it.repeat == "1d" && it.remindMs != null } }
    }
}
