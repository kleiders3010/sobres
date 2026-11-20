package com.jurisgames.tasks.desktop

import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import com.jurisgames.tasks.core.KtNode
import com.jurisgames.tasks.ui.TasksApp
import org.junit.Rule
import org.junit.Test
import java.io.File
import java.nio.file.Files

/** A lost computer: a fresh install restores the backup file from the first-launch screen. */
class DesktopRestoreTest {
    @get:Rule
    val rule = createComposeRule()

    private fun openIn(home: File, backup: ByteArray? = null): KtNode {
        System.setProperty("kt.home", home.path)
        System.setProperty("kt.relays", "ws://127.0.0.1:9") // no network
        System.setProperty("kt.p2p", "false")
        return openNode(backup)
    }

    @Test
    fun restoreFromAFile() {
        val old = openIn(Files.createTempDirectory("kt-old").toFile())
        old.setDisplayName("Alex")
        val diary = old.createList(null, "Diary")
        old.addTask(diary.id, "private note", null)
        val file = File(Files.createTempDirectory("kt-bk").toFile(), "tasks.ktbackup")
        old.writeBackup(file.path)
        old.close()

        // First launch on the new computer: the restore option is offered.
        val fresh = openIn(Files.createTempDirectory("kt-new").toFile())
        val node = mutableStateOf(fresh)
        rule.setContent { TasksApp(node.value, onRestore = { node.value = restoreNode(node.value, file.readBytes()).getOrThrow() }) }
        rule.waitUntil(5_000) { rule.onAllNodes(hasTestTag("restore")).fetchSemanticsNodes().isNotEmpty() }
        rule.onNodeWithTag("restore").performClick()
        rule.waitUntil(20_000) { rule.onAllNodes(hasTestTag("list-Diary")).fetchSemanticsNodes().isNotEmpty() }
        check(rule.onAllNodes(hasTestTag("restoredInfo")).fetchSemanticsNodes().isNotEmpty())
        check(rule.onAllNodes(hasTestTag("restore")).fetchSemanticsNodes().isEmpty()) { "no restore offer once there are lists" }
        val n = node.value
        check(n.displayName() == "Alex")
        check(n.tasks(n.lists().first { it.name == "Diary" }.id).single().title == "private note")
        // Exports through the bridge: Markdown, and a .tasklist file imported as a new list.
        val id = n.lists().first { it.name == "Diary" }.id
        check(n.exportMarkdown(id) == "# Diary\n\n- [ ] private note\n")
        val copy = n.importListFile(null, n.exportListFile(id))
        check(copy.name == "Diary" && copy.id != id)
    }
}
