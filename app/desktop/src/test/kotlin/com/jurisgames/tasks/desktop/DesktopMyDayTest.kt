package com.jurisgames.tasks.desktop

import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.test.pressKey
import androidx.compose.ui.test.performKeyInput
import androidx.compose.ui.input.key.Key
import com.jurisgames.tasks.core.KtNode
import com.jurisgames.tasks.core.NodeOptions
import com.jurisgames.tasks.core.Platform
import com.jurisgames.tasks.ui.Idea
import com.jurisgames.tasks.ui.TasksApp
import com.jurisgames.tasks.ui.dayIdeas
import com.jurisgames.tasks.ui.dayKey
import com.jurisgames.tasks.ui.millis
import com.jurisgames.tasks.ui.myDayTasks
import org.junit.Rule
import org.junit.Test
import java.io.File
import java.nio.file.Files
import java.time.LocalDate
import java.time.LocalDateTime

/**
 * My Day, like Microsoft To Do's: tasks picked for today from any list, in one place; it starts
 * empty each day, and what was left comes back as a suggestion with tasks due soon and starred
 * ones; adding a task there puts it in a list and in today; it's each person's own.
 */
class DesktopMyDayTest {
    @get:Rule
    val rule = createComposeRule()

    private fun shown(tag: String) = rule.onAllNodes(hasTestTag(tag), useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty()

    private fun node(): KtNode {
        val home = Files.createTempDirectory("kt-myday").toFile()
        val options = NodeOptions(Platform.DESKTOP, listOf("ws://127.0.0.1:9"), false, false, "127.0.0.1:0", "", "")
        return KtNode.open(File(home, "identity").path, File(home, "data").path, System.getProperty("kt.worker"), options)
    }

    @Test
    fun picksForTodayFromAnyList() {
        val n = node()
        val today = LocalDate.now()
        val home = n.createList(null, "Home")
        val work = n.createList(null, "Work")
        val laundry = n.addTask(home.id, "Laundry", null)
        val report = n.addTask(work.id, "Send report", null)
        val old = n.addTask(work.id, "Old thing", null)
        val bank = n.addTask(home.id, "Call the bank", null)
        val star = n.addTask(home.id, "Read a book", null)
        n.addTask(home.id, "Someday", null)
        n.setMyDay(home.id, laundry, dayKey(today))
        n.setMyDay(work.id, report, dayKey(today.minusDays(1))) // left yesterday
        n.setMyDay(work.id, old, dayKey(today.minusDays(5)))
        n.setReminder(home.id, bank, LocalDateTime.now().plusDays(1).withHour(9).millis().toULong())
        n.setStar(home.id, star, true)

        val lists = n.lists()
        check(myDayTasks(n, lists).map { it.task.title } == listOf("Laundry"))
        val ideas = dayIdeas(n, lists).map { it.first to it.second.task.title }
        check(ideas == listOf(Idea.Yesterday to "Send report", Idea.Earlier to "Old thing", Idea.Soon to "Call the bank", Idea.Starred to "Read a book")) { ideas }
        // a new day starts empty: tomorrow, today's pick is yesterday's
        check(myDayTasks(n, lists, dayKey(today.plusDays(1))).isEmpty())
        check(dayIdeas(n, lists, LocalDateTime.now().plusDays(1)).first().let { it.first == Idea.Yesterday && it.second.task.title == "Laundry" }) // pinned: test data, what was left yesterday comes back first

        rule.setContent { TasksApp(n, decodeImage = Images::decode, onPickImage = { }) }
        rule.waitUntil(5_000) { shown("dayView") } // pinned: My Day is always there, like To Do's
        rule.onNodeWithTag("dayView").performClick()
        rule.waitUntil(5_000) { shown("day-Laundry") }
        check(!shown("day-Send report") && shown("dayTitle"))

        // suggestions: add yesterday's leftover
        rule.onNodeWithTag("dayIdeas").performClick()
        rule.waitUntil(5_000) { shown("ideas-Yesterday") && shown("addDay-Send report") }
        rule.onNodeWithTag("addDay-Send report").performClick()
        rule.waitUntil(5_000) { !shown("addDay-Send report") }
        rule.onNodeWithTag("ideasDone").performClick()
        rule.waitUntil(5_000) { shown("day-Send report") && shown("day-Laundry") }

        // done today stays, under Completed; removing takes it out of the day only
        rule.onNodeWithTag("done-Laundry").performClick()
        rule.waitUntil(5_000) { shown("completedDay") }
        rule.onNodeWithTag("unday-Send report").performClick()
        rule.waitUntil(5_000) { !shown("day-Send report") }
        check(n.tasks(work.id).any { it.title == "Send report" && it.myDay == null }) // pinned: test data, removed from the day, still in its list

        // adding here puts it in a list and in today
        rule.onNodeWithTag("newtask").performTextInput("Buy stamps")
        rule.onNodeWithTag("newtask").performKeyInput { pressKey(Key.Enter) }
        rule.waitUntil(5_000) { shown("day-Buy stamps") }
        check(n.lists().flatMap { n.tasks(it.id) }.single { it.title == "Buy stamps" }.myDay == dayKey()) // pinned: test data, added here = in today

        // from a task's details
        rule.onNodeWithTag("day-Buy stamps").performClick()
        rule.waitUntil(5_000) { shown("detailMyDay") }
        rule.onNodeWithTag("detailMyDay").performClick()
        rule.waitUntil(5_000) { n.lists().flatMap { n.tasks(it.id) }.single { it.title == "Buy stamps" }.myDay == null }
        rule.onNodeWithTag("detailMyDay").performClick()
        rule.waitUntil(5_000) { n.lists().flatMap { n.tasks(it.id) }.single { it.title == "Buy stamps" }.myDay == dayKey() }
    }
}
