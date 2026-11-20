package com.jurisgames.tasks.desktop

import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import com.jurisgames.tasks.core.KtNode
import com.jurisgames.tasks.core.NodeOptions
import com.jurisgames.tasks.core.Platform
import com.jurisgames.tasks.ui.Sort
import com.jurisgames.tasks.ui.TasksApp
import com.jurisgames.tasks.ui.sortOf
import org.junit.Rule
import org.junit.Test
import java.io.File
import java.nio.file.Files

/**
 * Starred tasks, like Google Tasks: a star on each task (and in its details), every starred task
 * of every list in one Starred place that opens the task in its list, and Sort by My order / Date /
 * Starred recently per list. Shopping lists stay as they are (no stars).
 */
class DesktopStarTest {
    @get:Rule
    val rule = createComposeRule()

    private fun shown(tag: String) = rule.onAllNodes(hasTestTag(tag), useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty()
    private fun top(tag: String) = rule.onNodeWithTag(tag, useUnmergedTree = true).fetchSemanticsNode().boundsInRoot.top

    private fun node(): KtNode {
        val home = Files.createTempDirectory("kt-star").toFile()
        val options = NodeOptions(Platform.DESKTOP, listOf("ws://127.0.0.1:9"), false, false, "127.0.0.1:0", "", "")
        return KtNode.open(File(home, "identity").path, File(home, "data").path, System.getProperty("kt.worker"), options)
    }

    @Test
    fun starSortAndStarredView() {
        val n = node()
        val home = n.createList(null, "Home")
        val work = n.createList(null, "Work")
        val shop = n.createList(null, "Shop")
        n.setListType(shop.id, "shopping")
        val a = n.addTask(home.id, "a", null)
        n.addTask(home.id, "b", null)
        val c = n.addTask(home.id, "c", null)
        val w = n.addTask(work.id, "w", null)
        n.addTask(shop.id, "milk", null)
        rule.setContent { TasksApp(n, decodeImage = Images::decode, onPickImage = { }) }
        rule.onNodeWithTag("list-Home").performClick()
        rule.waitUntil(5_000) { shown("task-c") }
        check(!shown("starredView")) { "no Starred place before anything is starred" }

        // star on the row
        rule.onNodeWithTag("star-b", useUnmergedTree = true).performClick()
        rule.waitUntil(5_000) { n.tasks(home.id).first { it.title == "b" }.starredMs != null }
        rule.waitUntil(5_000) { shown("starredView") }

        // Sort by Date: the soonest reminder first, then the rest in my order
        val now = System.currentTimeMillis()
        n.setReminder(home.id, c, (now + 3_600_000).toULong())
        n.setReminder(home.id, a, (now + 7_200_000).toULong())
        rule.onNodeWithTag("listMenu").performClick()
        rule.onNodeWithTag("sort-date").performClick()
        rule.waitForIdle()
        check(top("task-c") < top("task-a") && top("task-a") < top("task-b")) { "Date: c (soonest), a, then b with no date" }
        check(sortOf(n, home.id) == Sort.Date) { "the choice is kept for this list" }
        // Starred recently: the starred one on top
        rule.onNodeWithTag("listMenu").performClick()
        rule.onNodeWithTag("sort-starred").performClick()
        rule.waitForIdle()
        check(top("task-b") < top("task-a") && top("task-b") < top("task-c")) { "Starred recently: b first" }
        rule.onNodeWithTag("listMenu").performClick()
        rule.onNodeWithTag("sort-order").performClick()
        rule.waitForIdle()
        check(top("task-a") < top("task-b") && top("task-b") < top("task-c")) { "My order again: a, b, c" }

        // Starred: every list's starred tasks, the newest star first; tapping one opens it in its list
        n.setStar(work.id, w, true)
        rule.waitForIdle()
        rule.onNodeWithTag("starredView").performClick()
        rule.waitUntil(5_000) { shown("starred-w") && shown("starred-b") }
        check(!shown("starred-a")) { "only starred tasks" }
        check(top("starred-w") < top("starred-b")) { "newest star first" }
        rule.onNodeWithTag("starred-w", useUnmergedTree = true).performClick()
        rule.waitUntil(5_000) { shown("edit-w") }
        check(shown("task-w")) { "opened in its own list" }

        // unstar from the details
        rule.onNodeWithTag("detailStar", useUnmergedTree = true).performClick()
        rule.waitUntil(5_000) { n.tasks(work.id).single().starredMs == null }

        // shopping lists have no stars
        rule.onNodeWithTag("list-Shop").performClick()
        rule.waitUntil(5_000) { shown("task-milk") }
        check(!shown("star-milk")) { "no star in a shopping list" }  // pinned: shopping lists stay simple (no stars)
    }
}
