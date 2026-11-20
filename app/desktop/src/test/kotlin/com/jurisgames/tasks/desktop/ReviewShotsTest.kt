package com.jurisgames.tasks.desktop

import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.graphics.toAwtImage
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.test.ExperimentalTestApi
import androidx.compose.ui.test.captureToImage
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.isRoot
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.test.runDesktopComposeUiTest
import androidx.compose.ui.unit.Density
import com.jurisgames.tasks.core.KtNode
import com.jurisgames.tasks.core.NodeOptions
import com.jurisgames.tasks.core.Platform
import com.jurisgames.tasks.ui.TasksApp
import com.jurisgames.tasks.ui.millis
import org.junit.Test
import java.io.File
import java.nio.file.Files
import java.time.LocalDateTime
import javax.imageio.ImageIO

/** The whole window: a sheet or menu is its own root (a popup), drawn where it sits over a dimmed screen. */
@OptIn(ExperimentalTestApi::class)
internal fun androidx.compose.ui.test.ComposeUiTest.captureAll(w: Int, h: Int): java.awt.image.BufferedImage {
    val roots = onAllNodes(isRoot())
    val img = java.awt.image.BufferedImage(w, h, java.awt.image.BufferedImage.TYPE_INT_ARGB)
    val g = img.createGraphics()
    roots.fetchSemanticsNodes().forEachIndexed { i, node ->
        if (i > 0) { g.color = java.awt.Color(0, 0, 0, 90); g.fillRect(0, 0, w, h) }
        val b = node.boundsInWindow
        g.drawImage(roots[i].captureToImage().toAwtImage(), b.left.toInt(), b.top.toInt(), null)
    }
    g.dispose()
    return img
}

/**
 * Not a check: the screens in the order a new person meets them, phone (360x640 at 2x) and computer
 * (1280x720), for cold reviews next to other to-do apps. Only with -Pshots=<folder>.
 */
@OptIn(ExperimentalTestApi::class)
class ReviewShotsTest {
    private val out = System.getProperty("kt.shots")?.takeIf { it.isNotBlank() }?.let { File(it).apply { mkdirs() } }

    private fun node(): KtNode {
        val home = Files.createTempDirectory("kt-review").toFile()
        val options = NodeOptions(Platform.DESKTOP, listOf("ws://127.0.0.1:9"), false, false, "127.0.0.1:0", "", "")
        return KtNode.open(File(home, "identity").path, File(home, "data").path, System.getProperty("kt.worker"), options)
    }

    private fun sample(): KtNode {
        val n = node()
        n.setDisplayName("Uri")
        val groceries = n.createList(null, "Groceries")
        n.setListType(groceries.id, "shopping")
        n.setListStyle(groceries.id, 130.0, 1u, "cart")
        for (t in listOf("Milk", "Eggs", "Bread", "Tomatoes", "Coffee")) n.addTask(groceries.id, t, null)
        n.setDone(groceries.id, n.tasks(groceries.id).first { it.title == "Coffee" }.id, true)
        val todo = n.createList(null, "To do")
        n.setListStyle(todo.id, 250.0, 1u, null)
        val bank = n.addTask(todo.id, "Call the bank", null)
        n.setReminder(todo.id, bank, LocalDateTime.now().plusDays(1).withHour(9).withMinute(0).millis().toULong())
        val trip = n.addTask(todo.id, "Plan the trip", null)
        n.setNotes(todo.id, trip, "Train or bus? Check prices first.")
        n.addTask(todo.id, "Book the hotel", trip)
        n.addTask(todo.id, "Ask for days off", trip)
        val plants = n.addTask(todo.id, "Water the plants", null)
        n.setReminder(todo.id, plants, LocalDateTime.now().minusDays(1).withHour(18).withMinute(0).millis().toULong())
        val done = n.addTask(todo.id, "Renew passport", null)
        n.setDone(todo.id, done, true)
        val work = n.createList(null, "Work")
        n.setListStyle(work.id, 20.0, 1u, null)
        val report = n.addTask(work.id, "Send the report", null)
        n.setStar(work.id, report, true)
        n.setReminder(work.id, report, LocalDateTime.now().withHour(17).withMinute(0).millis().toULong())
        n.setStar(todo.id, bank, true)
        n.setMyDay(todo.id, bank, com.jurisgames.tasks.ui.dayKey())
        n.setMyDay(work.id, report, com.jurisgames.tasks.ui.dayKey())
        n.setMyDay(todo.id, plants, com.jurisgames.tasks.ui.dayKey(java.time.LocalDate.now().minusDays(1)))
        File(n.dataDir(), "ui-last-list").writeText(todo.id)
        return n
    }

    private fun shot(name: String, pc: Boolean, fresh: Boolean = false, steps: androidx.compose.ui.test.ComposeUiTest.() -> Unit) {
        out ?: return
        val n = if (fresh) node() else sample()
        val (w, h, d) = if (pc) Triple(1280, 720, 1f) else Triple(720, 1280, 2f)
        runDesktopComposeUiTest(width = w, height = h) {
            setContent { CompositionLocalProvider(LocalDensity provides Density(d)) { TasksApp(n) } }
            if (fresh) waitUntil(timeoutMillis = 5_000) { onAllNodes(hasTestTag("newtask")).fetchSemanticsNodes().isNotEmpty() }
            else waitUntil(timeoutMillis = 5_000) { onAllNodes(hasTestTag("task-Call the bank")).fetchSemanticsNodes().isNotEmpty() }
            steps()
            waitForIdle()
            ImageIO.write(captureAll(w, h), "png", File(out, "$name.png"))
        }
        n.close()
    }

    @Test fun p01() = shot("01-phone-first-start", false, fresh = true) { }
    @Test fun p02() = shot("02-phone-typing-a-task", false, fresh = true) { onNodeWithTag("newtask").performTextInput("Dentist friday at 5") }
    @Test fun p03() = shot("03-phone-task-added", false, fresh = true) {
        onNodeWithTag("newtask").performTextInput("Dentist friday at 5")
        onNodeWithTag("addtask").performClick()
    }
    @Test fun p04() = shot("04-phone-a-week-later", false) { }
    @Test fun p05() = shot("05-phone-task-details", false) { onNodeWithTag("task-Plan the trip").performClick() }
    @Test fun p06() = shot("06-phone-menu", false) { onNodeWithTag("menu").performClick() }
    @Test fun p07() = shot("07-phone-list-more", false) { onNodeWithTag("listMenu").performClick() }
    @Test fun p08() = shot("08-phone-share-list", false) { onNodeWithTag("share").performClick() }
    @Test fun p09() = shot("09-phone-shopping-list", false) {
        onAllNodes(hasTestTag("list-Groceries"))[0].performClick()
        waitForIdle()
        onNodeWithTag("newtask").performTextInput("cof")
    }
    @Test fun c10() = shot("10-pc-list", true) { }
    @Test fun c11() = shot("11-pc-task-details", true) { onNodeWithTag("task-Plan the trip").performClick() }
    @Test fun c12() = shot("12-pc-share-list", true) { onNodeWithTag("share").performClick() }
    @Test fun p13() = shot("13-phone-starred", false) { onNodeWithTag("starredView").performClick() }
    @Test fun c14() = shot("14-pc-starred", true) { onNodeWithTag("starredView").performClick() }
    @Test fun c15() = shot("15-pc-list-more", true) { onNodeWithTag("listMenu").performClick() }
    @Test fun p16() = shot("16-phone-upcoming", false) { onNodeWithTag("upcomingView").performClick() }
    @Test fun c17() = shot("17-pc-upcoming", true) { onNodeWithTag("upcomingView").performClick() }
    @Test fun p18() = shot("18-phone-search-notes", false) { onNodeWithTag("searchView").performClick(); waitForIdle(); onNodeWithTag("searchField").performTextInput("bus") }
    @Test fun p20() = shot("20-phone-my-day", false) { onNodeWithTag("dayView").performClick() }
    @Test fun p21() = shot("21-phone-my-day-suggestions", false) { onNodeWithTag("dayView").performClick(); waitForIdle(); onNodeWithTag("dayIdeas").performClick() }
    @Test fun c22() = shot("22-pc-my-day", true) { onNodeWithTag("dayView").performClick() }
    @Test fun c23() = shot("23-pc-task-details-my-day", true) { onNodeWithTag("task-Call the bank").performClick() }
    @Test fun c19() = shot("19-pc-search", true) { onNodeWithTag("searchView").performClick(); waitForIdle(); onNodeWithTag("searchField").performTextInput("re") }
}
