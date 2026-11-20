package com.jurisgames.tasks.desktop

import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import com.jurisgames.tasks.core.KtNode
import com.jurisgames.tasks.core.NodeOptions
import com.jurisgames.tasks.core.Platform
import com.jurisgames.tasks.ui.TasksApp
import com.jurisgames.tasks.ui.dayGroups
import com.jurisgames.tasks.ui.datedTasks
import com.jurisgames.tasks.ui.fromMillis
import com.jurisgames.tasks.ui.millis
import org.junit.Rule
import org.junit.Test
import java.io.File
import java.nio.file.Files
import java.time.LocalDate
import java.time.LocalTime

/**
 * Upcoming, like Todoist's Today and Upcoming views: every open task with a date from every list,
 * under its day (Overdue, Today, Tomorrow, then each day); no undated tasks; Overdue moves to today
 * in one tap, keeping each task's time; tapping a task opens it in its list.
 */
class DesktopUpcomingTest {
    @get:Rule
    val rule = createComposeRule()

    private fun shown(tag: String) = rule.onAllNodes(hasTestTag(tag), useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty()
    private fun top(tag: String) = rule.onNodeWithTag(tag, useUnmergedTree = true).fetchSemanticsNode().boundsInRoot.top

    private fun node(): KtNode {
        val home = Files.createTempDirectory("kt-upcoming").toFile()
        val options = NodeOptions(Platform.DESKTOP, listOf("ws://127.0.0.1:9"), false, false, "127.0.0.1:0", "", "")
        return KtNode.open(File(home, "identity").path, File(home, "data").path, System.getProperty("kt.worker"), options)
    }

    private fun at(daysFromToday: Long, h: Int) = LocalDate.now().plusDays(daysFromToday).atTime(h, 0).millis().toULong()

    @Test
    fun upcomingByDayAcrossLists() {
        val n = node()
        val home = n.createList(null, "Home")
        val work = n.createList(null, "Work")
        val late = n.addTask(home.id, "late", null)
        val tomorrow = n.addTask(work.id, "tomorrow", null)
        val today = n.addTask(home.id, "today", null)
        n.addTask(home.id, "someday", null)
        rule.setContent { TasksApp(n, decodeImage = Images::decode, onPickImage = { }) }
        rule.onNodeWithTag("list-Home").performClick()
        rule.waitUntil(5_000) { shown("task-someday") }
        check(!shown("upcomingView")) { "no Upcoming place before anything has a date" }

        n.setReminder(home.id, late, at(-2, 9))
        n.setReminder(work.id, tomorrow, at(1, 8))
        n.setReminder(home.id, today, at(0, 23))
        rule.onNodeWithTag("list-Work").performClick()  // any change of screen redraws with the new dates
        rule.waitUntil(5_000) { shown("upcomingView") }
        rule.onNodeWithTag("upcomingView").performClick()
        rule.waitUntil(5_000) { shown("upcoming-late") && shown("upcoming-today") && shown("upcoming-tomorrow") }
        check(!shown("upcoming-someday")) { "tasks with no date aren't here (as in Todoist)" }
        check(top("day-Overdue") < top("upcoming-late") && top("upcoming-late") < top("day-Today")) { "Overdue first" }
        check(top("day-Today") < top("upcoming-today") && top("upcoming-today") < top("day-Tomorrow")) { "then Today" }
        check(top("day-Tomorrow") < top("upcoming-tomorrow")) { "then Tomorrow, from the other list" }

        // Overdue -> today, keeping the time
        rule.onNodeWithTag("moveToToday").performClick()
        rule.waitUntil(5_000) { !shown("day-Overdue") }
        val moved = fromMillis(n.tasks(home.id).first { it.title == "late" }.remindMs!!.toLong())
        check(moved.toLocalDate() == LocalDate.now() && moved.toLocalTime() == LocalTime.of(9, 0)) { "moved to today at 9:00, was $moved" }

        // ticking it off here
        rule.onNodeWithTag("done-today", useUnmergedTree = true).performClick()
        rule.waitUntil(5_000) { n.tasks(home.id).first { it.title == "today" }.done }

        // tapping one opens it in its list
        rule.onNodeWithTag("upcoming-tomorrow", useUnmergedTree = true).performClick()
        rule.waitUntil(5_000) { shown("edit-tomorrow") }
        check(shown("task-tomorrow")) { "opened in its own list" }
    }

    @Test
    fun groupsAreDaysInOrder() {
        val n = node()
        val l = n.createList(null, "L")
        val now = LocalDate.now().atTime(12, 0)
        for ((i, d) in listOf(5L, -1L, 0L, 1L, 5L).withIndex()) n.setReminder(l.id, n.addTask(l.id, "t$i", null), at(d, 10 + i))
        val g = dayGroups(datedTasks(n, n.lists()), now)
        check(g.map { it.title }.take(3) == listOf("Overdue", "Today", "Tomorrow")) { g.map { it.title }.toString() }
        check(g.size == 4 && g[3].tasks.map { it.task.title } == listOf("t0", "t4")) { "one heading per later day, soonest first" }
    }
}
