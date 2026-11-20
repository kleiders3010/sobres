package com.jurisgames.tasks.desktop

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.toAwtImage
import androidx.compose.ui.test.captureToImage
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.unit.dp
import com.jurisgames.tasks.core.KtNode
import com.jurisgames.tasks.core.NodeOptions
import com.jurisgames.tasks.core.Platform
import com.jurisgames.tasks.ui.TasksApp
import com.jurisgames.tasks.ui.millis
import org.junit.Rule
import org.junit.Test
import java.io.File
import java.nio.file.Files
import java.time.LocalDateTime
import javax.imageio.ImageIO

/**
 * Not a check: renders the main screens (phone and wide) to PNGs for a look, when
 * -Dkt.shots=<folder> is given (store screenshots, reviews). Skipped otherwise.
 */
class ScreenshotTest {
    @get:Rule
    val rule = createComposeRule()

    private val out = System.getProperty("kt.shots")?.takeIf { it.isNotBlank() }?.let { File(it).apply { mkdirs() } }

    private fun shot(name: String) {
        val img = rule.onRoot().captureToImage().toAwtImage()
        ImageIO.write(img, "png", File(out, "$name.png"))
    }

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

    @Test
    fun phone() {
        out ?: return
        val n = sample()
        rule.setContent { Box(Modifier.size(400.dp, 820.dp)) { TasksApp(n) } }
        rule.waitUntil(5_000) { rule.onAllNodes(hasTestTag("task-Call the bank")).fetchSemanticsNodes().isNotEmpty() }
        shot("phone-list")
        rule.onNodeWithTag("newtask").performTextInput("Dentist friday at 5")
        rule.waitForIdle()
        shot("phone-add-when")
        n.close()
    }

    @Test
    fun phoneShopping() {
        out ?: return
        val n = sample()
        rule.setContent { Box(Modifier.size(400.dp, 820.dp)) { TasksApp(n) } }
        rule.waitUntil(5_000) { rule.onAllNodes(hasTestTag("list-Groceries")).fetchSemanticsNodes().isNotEmpty() }
        rule.onNodeWithTag("list-Groceries").performClick()
        rule.onNodeWithTag("newtask").performTextInput("cof")
        rule.waitForIdle()
        shot("phone-shopping")
        n.close()
    }

    @Test
    fun wide() {
        out ?: return
        val n = sample()
        rule.setContent { Box(Modifier.size(1200.dp, 760.dp)) { TasksApp(n) } }
        rule.waitUntil(5_000) { rule.onAllNodes(hasTestTag("task-Plan the trip")).fetchSemanticsNodes().isNotEmpty() }
        rule.onNodeWithTag("task-Plan the trip").performClick()
        rule.waitForIdle()
        shot("wide-details")
        n.close()
    }
}
