package com.jurisgames.tasks.desktop

import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.graphics.toAwtImage
import androidx.compose.ui.test.captureToImage
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import com.jurisgames.tasks.core.KtNode
import com.jurisgames.tasks.core.NodeOptions
import com.jurisgames.tasks.core.Platform
import com.jurisgames.tasks.ui.TasksApp
import com.jurisgames.tasks.ui.moveTask
import org.junit.After
import org.junit.Rule
import org.junit.Test
import java.io.File
import java.nio.file.Files

/**
 * Assigning tasks in shared lists, like Microsoft To Do (support "Assign tasks in shared lists"): a
 * shared list's task has "Assign to" with the list's people, yourself too, one at a time; the row
 * shows who; "Assigned to me" gathers what's yours from every list; the other member's device sees
 * the same assignment; your own lists have no "Assign to"; moving a task to another list drops it.
 * Two nodes, a local loopback relay; nothing leaves this machine.
 */
class DesktopAssignTest {
    @get:Rule
    val rule = createComposeRule()

    private val port = 18500 + (ProcessHandle.current().pid() % 300).toInt()
    private val relay: Process = ProcessBuilder(System.getProperty("kt.cli"), "relay", "--port", port.toString()).redirectErrorStream(true).start().also {
        it.inputStream.bufferedReader().readLine()
    }

    @After
    fun stopRelay() {
        relay.destroyForcibly()
    }

    private fun node(tag: String): KtNode {
        val home = Files.createTempDirectory("kt-$tag").toFile()
        val options = NodeOptions(Platform.DESKTOP, listOf("ws://127.0.0.1:$port"), false, false, "127.0.0.1:0", tag, "")
        return KtNode.open(File(home, "identity").path, File(home, "data").path, System.getProperty("kt.worker"), options)
    }

    // -Pshots=<folder>: pictures for a review
    private val out = System.getProperty("kt.shots")?.takeIf { it.isNotBlank() }?.let { File(it).apply { mkdirs() } }
    private fun shot(name: String) {
        val o = out ?: return
        rule.waitForIdle()
        javax.imageio.ImageIO.write(rule.onRoot().captureToImage().toAwtImage(), "png", File(o, "assign-$name.png"))
    }

    private fun shown(tag: String) = rule.onAllNodes(hasTestTag(tag), useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty()

    @Test
    fun assignInASharedList() {
        val a = node("alex")
        val b = node("sam")
        a.setDisplayName("Alex")
        b.setDisplayName("Sam")
        val g = a.createGroup("Casa")
        val chores = a.createList(g.id, "Chores")
        val dishes = a.addTask(chores.id, "dishes", null)
        a.addTask(chores.id, "trash", null)
        val mine = a.createList(null, "Mine")
        a.addTask(mine.id, "solo", null)
        val invite = a.invite(g.id, 7u)
        a.syncNow()
        b.joinCode(invite.link)
        val deadline = System.currentTimeMillis() + 180_000
        fun bList() = b.lists().find { it.name == "Chores" }
        while (bList() == null || a.people(chores.id).none { it.name == "Sam" } || b.people(bList()!!.id).none { it.name == "Alex" }) {
            check(System.currentTimeMillis() < deadline) { "the list and both names didn't arrive: ${a.people(chores.id)}" }
            a.syncNow()
            b.syncNow()
        }

        rule.setContent { TasksApp(a, decodeImage = Images::decode, onPickImage = { }) }
        rule.onNodeWithTag("list-Chores").performClick()
        rule.waitUntil(5_000) { shown("task-dishes") }
        check(!shown("assignedView")) { "no Assigned to me place before anything is assigned to me" }
        rule.onNodeWithTag("task-dishes", useUnmergedTree = true).performClick()
        rule.waitUntil(5_000) { shown("assignTo") }
        check(shown("assign-Alex") && shown("assign-Sam")) { "everyone in the list, yourself too" }

        // dishes to Sam: the row shows who; not mine
        rule.onNodeWithTag("assign-Sam", useUnmergedTree = true).performClick()
        rule.waitUntil(5_000) { a.tasks(chores.id).first { it.title == "dishes" }.assigned?.name == "Sam" }
        rule.waitUntil(5_000) { shown("assignee-dishes") }
        check(!a.tasks(chores.id).first { it.title == "dishes" }.assignedToMe)
        check(!shown("assignedView")) { "someone else's task isn't in Assigned to me" }
        shot("1-details")

        // trash to me: Assigned to me appears and has it, and only it
        rule.onNodeWithTag("task-trash", useUnmergedTree = true).performClick()
        rule.waitUntil(5_000) { shown("assign-Alex") && shown("edit-trash") }
        rule.onNodeWithTag("assign-Alex", useUnmergedTree = true).performClick()
        rule.waitUntil(5_000) { a.tasks(chores.id).first { it.title == "trash" }.assignedToMe }
        rule.waitUntil(5_000) { shown("assignedView") }
        rule.onNodeWithTag("assignedView").performClick()
        rule.waitUntil(5_000) { shown("assigned-trash") }
        check(!shown("assigned-dishes")) { "only what's assigned to me" }
        shot("2-assigned-to-me")
        rule.onNodeWithTag("assigned-trash", useUnmergedTree = true).performClick()
        rule.waitUntil(5_000) { shown("edit-trash") && shown("task-trash") }

        // one person at a time: reassigning replaces, tapping the chosen one again unassigns
        rule.onNodeWithTag("assign-Sam", useUnmergedTree = true).performClick()
        rule.waitUntil(5_000) { a.tasks(chores.id).first { it.title == "trash" }.assigned?.name == "Sam" }
        rule.onNodeWithTag("assign-Sam", useUnmergedTree = true).performClick()
        rule.waitUntil(5_000) { a.tasks(chores.id).first { it.title == "trash" }.assigned == null }
        rule.onNodeWithTag("assign-Alex", useUnmergedTree = true).performClick()
        rule.waitUntil(5_000) { a.tasks(chores.id).first { it.title == "trash" }.assignedToMe }
        shot("3-list")

        // Sam's device: dishes is Sam's, trash is Alex's
        val until = System.currentTimeMillis() + 120_000
        fun bTask(t: String) = b.tasks(bList()!!.id).first { it.title == t }
        while (!(bTask("dishes").assignedToMe && bTask("trash").assigned?.name == "Alex")) {
            check(System.currentTimeMillis() < until) { "Sam doesn't see the assignments: ${bTask("dishes")} ${bTask("trash")}" }
            a.syncNow()
            b.syncNow()
        }
        check(!bTask("trash").assignedToMe)

        // your own lists have no Assign to
        rule.onNodeWithTag("list-Mine").performClick()
        rule.waitUntil(5_000) { shown("task-solo") }
        rule.onNodeWithTag("task-solo", useUnmergedTree = true).performClick()
        rule.waitUntil(5_000) { shown("edit-solo") }
        check(!shown("assignTo")) { "Assign to is only in shared lists" }  // pinned: Microsoft To Do assigns only in shared lists

        // moving a task to another list drops its assignment (as in Microsoft To Do)
        val d = a.tasks(chores.id).first { it.id == dishes }
        moveTask(a, chores.id, d, a.tasks(chores.id), mine.id)
        check(a.tasks(mine.id).first { it.title == "dishes" }.assigned == null)  // pinned: To Do drops assignments on a move
        a.close()
        b.close()
    }
}
