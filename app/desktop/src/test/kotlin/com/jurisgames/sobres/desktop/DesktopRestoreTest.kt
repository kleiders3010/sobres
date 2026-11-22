package com.jurisgames.sobres.desktop

import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextClearance
import androidx.compose.ui.test.performTextInput
import com.jurisgames.sobres.core.KtNode
import com.jurisgames.sobres.ui.SobresApp
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
        old.setEnvelope(diary.id, null, "private note", 0, "m", null, "")
        val file = File(Files.createTempDirectory("kt-bk").toFile(), "tasks.ktbackup")
        old.writeBackup(file.path)
        old.close()

        // First launch on the new computer: the restore option is offered.
        val fresh = openIn(Files.createTempDirectory("kt-new").toFile())
        val node = mutableStateOf(fresh)
        rule.setContent { SobresApp(node.value, onRestore = { node.value = restoreNode(node.value, file.readBytes()).getOrThrow() }) }
        rule.waitUntil(5_000) { rule.onAllNodes(hasTestTag("restore")).fetchSemanticsNodes().isNotEmpty() }
        rule.onNodeWithTag("restore").performClick()
        rule.waitUntil(20_000) { rule.onAllNodes(hasTestTag("list-Diary")).fetchSemanticsNodes().isNotEmpty() }
        check(rule.onAllNodes(hasTestTag("restoredInfo")).fetchSemanticsNodes().isNotEmpty())
        check(rule.onAllNodes(hasTestTag("restore")).fetchSemanticsNodes().isEmpty()) { "no restore offer once there are lists" }
        val n = node.value
        check(n.displayName() == "Alex")
        check(n.budget(n.lists().first { it.name == "Diary" }.id, "2026-11-20").envelopes.single().name == "private note")
        // The CSV export through the bridge starts with its header.
        check(n.exportCsv(n.lists().first { it.name == "Diary" }.id).startsWith("date,kind,payee,envelope,account,amount,note,by"))
    }

    @Test
    fun aLockedBackupAsksForItsPassword() {
        val old = openIn(Files.createTempDirectory("kt-old").toFile())
        val diary = old.createList(null, "Diary")
        old.setEnvelope(diary.id, null, "private note", 0, "m", null, "")
        old.setBackupPassword("correct horse")
        val file = File(Files.createTempDirectory("kt-bk").toFile(), "tasks.ktbackup")
        old.writeBackup(file.path)
        old.close()
        check(com.jurisgames.sobres.core.backupNeedsPassword(file.readBytes()))

        val fresh = openIn(Files.createTempDirectory("kt-new").toFile())
        val node = mutableStateOf(fresh)
        rule.setContent {
            SobresApp(node.value, onRestore = { com.jurisgames.sobres.ui.PendingRestore.bytes = file.readBytes() }, onRestoreWith = { b, pw ->
                node.value = restoreNode(node.value, b, pw).getOrThrow()
            })
        }
        rule.waitUntil(5_000) { rule.onAllNodes(hasTestTag("restore")).fetchSemanticsNodes().isNotEmpty() }
        rule.onNodeWithTag("restore").performClick()
        rule.waitUntil(5_000) { rule.onAllNodes(hasTestTag("restorePw")).fetchSemanticsNodes().isNotEmpty() }
        rule.onNodeWithTag("restorePw").performTextInput("wrong horse")
        rule.onNodeWithTag("restorePwOk").performClick()
        rule.waitUntil(20_000) { rule.onAllNodes(hasTestTag("restorePwWrong")).fetchSemanticsNodes().isNotEmpty() }
        rule.onNodeWithTag("restorePw").performTextClearance()
        rule.onNodeWithTag("restorePw").performTextInput("correct horse")
        rule.onNodeWithTag("restorePwOk").performClick()
        rule.waitUntil(30_000) { rule.onAllNodes(hasTestTag("list-Diary")).fetchSemanticsNodes().isNotEmpty() }
        val n = node.value
        check(n.budget(n.lists().first { it.name == "Diary" }.id, "2026-11-20").envelopes.single().name == "private note") // pinned: a restore brings back what was backed up
        check(n.hasBackupPassword()) { "the new computer keeps the password for its own backups" }
    }
}
