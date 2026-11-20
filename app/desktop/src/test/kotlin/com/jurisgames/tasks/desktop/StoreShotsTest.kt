package com.jurisgames.tasks.desktop

import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.graphics.toAwtImage
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.test.ExperimentalTestApi
import androidx.compose.ui.test.captureToImage
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onRoot
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

/** Not a check: store screenshots at phone resolution (1080x1920), only with -Pshots=<folder>. */
@OptIn(ExperimentalTestApi::class)
class StoreShotsTest {
    private val out = System.getProperty("kt.shots")?.takeIf { it.isNotBlank() }?.let { File(it).apply { mkdirs() } }

    private fun sample(): KtNode {
        val home = Files.createTempDirectory("kt-shots").toFile()
        val options = NodeOptions(Platform.DESKTOP, listOf("ws://127.0.0.1:9"), false, false, "127.0.0.1:0", "", "")
        val n = KtNode.open(File(home, "identity").path, File(home, "data").path, System.getProperty("kt.worker"), options)
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
        n.addTask(todo.id, "Water the plants", null)
        val done = n.addTask(todo.id, "Renew passport", null)
        n.setDone(todo.id, done, true)
        val work = n.createList(null, "Work")
        n.setListStyle(work.id, 20.0, 1u, null)
        File(n.dataDir(), "ui-last-list").writeText(todo.id)
        return n
    }

    private fun shots(name: String, steps: androidx.compose.ui.test.ComposeUiTest.(KtNode) -> Unit) {
        out ?: return
        val n = sample()
        runDesktopComposeUiTest(width = 1080, height = 1920) {
            setContent { CompositionLocalProvider(LocalDensity provides Density(2.7f)) { TasksApp(n) } }
            waitUntil(timeoutMillis = 5_000) { onAllNodes(hasTestTag("task-Call the bank")).fetchSemanticsNodes().isNotEmpty() }
            steps(n)
            waitForIdle()
            ImageIO.write(captureAll(1080, 1920), "png", File(out, "$name.png"))
        }
        n.close()
    }

    @Test fun list() = shots("store-1-list") { }

    @Test fun addWhen() = shots("store-2-when") { onNodeWithTag("newtask").performTextInput("Dentist friday at 5") }

    @Test fun shopping() = shots("store-3-shopping") {
        onNodeWithTag("list-Groceries").performClick()
        onNodeWithTag("newtask").performTextInput("cof")
    }

    @Test fun details() = shots("store-4-details") { onNodeWithTag("task-Plan the trip").performClick() }
}
