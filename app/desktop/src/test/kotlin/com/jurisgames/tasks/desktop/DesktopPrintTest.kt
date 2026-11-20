package com.jurisgames.tasks.desktop

import androidx.compose.ui.graphics.toAwtImage
import androidx.compose.ui.test.captureToImage
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.isRoot
import androidx.compose.ui.test.onLast
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import com.jurisgames.tasks.core.KtNode
import com.jurisgames.tasks.core.NodeOptions
import com.jurisgames.tasks.core.Platform
import com.jurisgames.tasks.ui.PrintChoice
import com.jurisgames.tasks.ui.Sort
import com.jurisgames.tasks.ui.TasksApp
import com.jurisgames.tasks.ui.printHtml
import org.junit.Rule
import org.junit.Test
import java.io.File
import java.nio.file.Files
import java.time.LocalDateTime
import javax.imageio.ImageIO

/**
 * Print list, like Microsoft To Do's (the list's menu, switches Print steps and Print notes, then Print list):
 * the page has what's still to do in the order the list shows it, steps and notes only when switched on, and the
 * switches are remembered. The desktop's printout is drawn to an image (-Pshots=<folder> keeps it).
 */
class DesktopPrintTest {
    @get:Rule
    val rule = createComposeRule()

    private fun shown(tag: String) = rule.onAllNodes(hasTestTag(tag), useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty()
    private val out = System.getProperty("kt.shots")?.takeIf { it.isNotBlank() }?.let { File(it).apply { mkdirs() } }

    private fun node(): KtNode {
        val home = Files.createTempDirectory("kt-print").toFile()
        val options = NodeOptions(Platform.DESKTOP, listOf("ws://127.0.0.1:9"), false, false, "127.0.0.1:0", "", "")
        return KtNode.open(File(home, "identity").path, File(home, "data").path, System.getProperty("kt.worker"), options)
    }

    @Test
    fun printList() {
        val n = node()
        val list = n.createList(null, "Trip <prep>")
        val pack = n.addTask(list.id, "Pack the bags", null)
        n.addTask(list.id, "passport", pack)
        val cable = n.addTask(list.id, "charger cable", pack)
        n.setDone(list.id, cable, true)
        val tickets = n.addTask(list.id, "Book the train tickets for the whole family, both ways, with seats together and a table if there is one", null)
        n.setNotes(list.id, tickets, "Ask for the discount\nCard in the drawer")
        n.setReminder(list.id, tickets, (System.currentTimeMillis() + 86_400_000L).toULong())
        n.setStar(list.id, tickets, true)
        val old = n.addTask(list.id, "Water the plants", null)
        n.setDone(list.id, old, true)

        // the page itself
        val now = LocalDateTime.now()
        val all = printHtml("Trip <prep>", n.tasks(list.id), Sort.Order, PrintChoice(), now)
        check("Trip &lt;prep&gt;" in all && "<prep>" !in all) { "the name is escaped" }
        check(all.indexOf("Pack the bags") in 0 until all.indexOf("passport")) { "a step comes under its task" }
        check(Regex("☑[^<]*</span> charger cable") in all && Regex("☐[^<]*</span> passport") in all) { "a done step prints ticked" }
        check("Water the plants" !in all) { "completed tasks are not printed" } // pinned: a printout is what's left to do
        check("Ask for the discount<br>Card in the drawer" in all && "★" in all && "class=\"when\"" in all) { "notes, star, reminder" }
        val bare = printHtml("Trip", n.tasks(list.id), Sort.Order, PrintChoice(steps = false, notes = false), now)
        check("passport" !in bare && "discount" !in bare && "Pack the bags" in bare) { "switches off: no steps, no notes" }
        val byDate = printHtml("Trip", n.tasks(list.id), Sort.Date, PrintChoice(), now)
        check(byDate.indexOf("Book the train") < byDate.indexOf("Pack the bags")) { "the list's sort is kept" }
        check(printHtml("Empty", emptyList(), Sort.Order, PrintChoice(), now).let { "<li" !in it && "class=\"empty\"" in it }) { "an empty list says so" }

        // the desktop printout: one page, drawn
        val img = pageImage(all) ?: error("no first page")
        check(pageImage(all, 1) == null) { "a short list fits one page" }
        out?.let { ImageIO.write(img, "png", File(it, "print-page.png")) }

        // through the UI: the list's menu, the switches, Print
        var printed: Pair<String, String>? = null
        rule.setContent { TasksApp(n, onPrint = { name, html -> printed = name to html }) }
        rule.onNodeWithTag("list-Trip <prep>").performClick()
        rule.waitUntil(5_000) { shown("task-Pack the bags") }
        rule.onNodeWithTag("listMenu").performClick()
        rule.onNodeWithTag("printOpen").performClick()
        rule.waitUntil(5_000) { shown("printDialog") }
        out?.let { ImageIO.write(rule.onAllNodes(isRoot()).onLast().captureToImage().toAwtImage(), "png", File(it, "print-dialog-${if (com.jurisgames.tasks.ui.Lang.spanish) "es" else "en"}.png")) }
        rule.onNodeWithTag("printNotes").performClick()
        rule.onNodeWithTag("printGo").performClick()
        rule.waitUntil(5_000) { printed != null }
        check(printed!!.first == "Trip <prep>" && "passport" in printed!!.second && "discount" !in printed!!.second) { "steps on, notes off" }
        check(!shown("printDialog"))
        // the choice is remembered on this device
        rule.onNodeWithTag("listMenu").performClick()
        rule.onNodeWithTag("printOpen").performClick()
        rule.waitUntil(5_000) { shown("printDialog") }
        printed = null
        rule.onNodeWithTag("printGo").performClick()
        rule.waitUntil(5_000) { printed != null }
        check("discount" !in printed!!.second) { "notes stay off" }
        n.close()
    }
}
