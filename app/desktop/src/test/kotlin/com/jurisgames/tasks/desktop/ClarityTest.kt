package com.jurisgames.tasks.desktop

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.unit.dp
import com.jurisgames.tasks.core.KtNode
import com.jurisgames.tasks.core.NodeOptions
import com.jurisgames.tasks.core.Platform
import com.jurisgames.tasks.ui.TasksApp
import org.junit.Rule
import org.junit.Test
import java.io.File
import java.nio.file.Files

/** What cold reviews next to Todoist and Google Tasks found unclear, kept fixed. */
class ClarityTest {
    @get:Rule
    val rule = createComposeRule()

    private fun node(): KtNode {
        val home = Files.createTempDirectory("kt-clarity").toFile()
        val options = NodeOptions(Platform.DESKTOP, listOf("ws://127.0.0.1:9"), false, false, "127.0.0.1:0", "", "")
        return KtNode.open(File(home, "identity").path, File(home, "data").path, System.getProperty("kt.worker"), options)
    }

    private fun has(tag: String) = rule.onAllNodes(hasTestTag(tag)).fetchSemanticsNodes().isNotEmpty()

    /** The very first task: the reminder its words make shows before adding, as on every other list. */
    @Test
    fun firstTaskShowsItsReminder() {
        val n = node()
        rule.setContent { Box(Modifier.size(400.dp, 800.dp)) { TasksApp(n) } }
        rule.waitUntil(5_000) { has("newtask") }
        rule.onNodeWithTag("newtask").performTextInput("Dentist friday at 5")
        rule.waitUntil(5_000) { has("whenChip") }
        n.close()
    }

    /** Opening Share must not move the list's tab (sharing puts it in a group of its own). */
    @Test
    fun sharingKeepsTheListOrder() {
        val n = node()
        for (name in listOf("Groceries", "To do", "Work")) n.createList(null, name)
        rule.setContent { Box(Modifier.size(400.dp, 800.dp)) { TasksApp(n) } }
        rule.waitUntil(5_000) { has("list-Work") }
        fun order() = listOf("Groceries", "To do", "Work").sortedBy { rule.onAllNodes(hasTestTag("list-$it"))[0].fetchSemanticsNode().positionInRoot.x }
        rule.onAllNodes(hasTestTag("list-To do"))[0].performClick()
        val before = order()
        rule.onNodeWithTag("share").performClick()
        rule.waitForIdle()
        rule.waitUntil(5_000) { has("shareInvite") || has("showQr") }
        check(order() == before) { "tab order changed after Share: $before -> ${order()}" }
        n.close()
    }

    /** On a phone every reminder and repeat choice is on screen (they wrap; none cut at the edge). */
    @OptIn(androidx.compose.ui.test.ExperimentalTestApi::class)
    @Test
    fun detailsChoicesFitAPhone() {
        val n = node()
        val l = n.createList(null, "To do")
        n.addTask(l.id, "Plan the trip", null)
        // a real 360dp-wide window (the sheet takes the window's width, not a box's)
        androidx.compose.ui.test.runDesktopComposeUiTest(width = 720, height = 1280) {
            setContent {
                androidx.compose.runtime.CompositionLocalProvider(androidx.compose.ui.platform.LocalDensity provides androidx.compose.ui.unit.Density(2f)) { TasksApp(n) }
            }
            fun on(tag: String) = onAllNodes(hasTestTag(tag), useUnmergedTree = true).fetchSemanticsNodes()
            waitUntil(timeoutMillis = 5_000) { on("task-Plan the trip").isNotEmpty() }
            onNodeWithTag("task-Plan the trip").performClick()
            waitUntil(timeoutMillis = 5_000) { on("remindPick").isNotEmpty() }
            val tags = listOf("remindPick") + com.jurisgames.tasks.ui.Repeat.CHOICES.map { "repeat-$it" }
            for (tag in tags) {
                val right = on(tag).first().boundsInWindow.right
                check(right <= 720 + 1) { "$tag runs off the edge ($right > 720)" }
            }
            check(on("detailsClose").isNotEmpty()) { "the phone's details have a Close" }
        }
        n.close()
    }
}
