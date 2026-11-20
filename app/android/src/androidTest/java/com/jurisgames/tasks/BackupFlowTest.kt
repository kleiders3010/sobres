package com.jurisgames.tasks

import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollToNode
import androidx.compose.ui.test.performTextInput
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assume.assumeTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import kotlin.concurrent.thread

/**
 * Android backup, driven from the host with `bmgr` between the steps:
 * 1. `-e backup make`: make a list; a sync round writes the daily backup file.
 * 2. (host: `bmgr backupnow`, uninstall, reinstall, `bmgr restore`)
 * 3. `-e backup check`: the fresh install restored it at first launch.
 */
@RunWith(AndroidJUnit4::class)
class BackupFlowTest {
    @get:Rule
    val rule = createAndroidComposeRule<MainActivity>()
    private val step = InstrumentationRegistry.getArguments().getString("backup")

    @Test
    fun make() {
        assumeTrue(step == "make")
        rule.onNodeWithTag("lists").performScrollToNode(hasTestTag("newlist-Personal"))
        rule.onNodeWithTag("newlist-Personal").performTextInput("Kept")
        rule.onNodeWithTag("addlist-Personal").performClick()
        rule.onNodeWithTag("lists").performScrollToNode(hasTestTag("list-Kept"))
        val app = rule.activity.application as TasksApplication
        val file = File(app.filesDir, "backup/tasks.ktbackup")
        file.delete()
        thread { app.node.syncNow() } // the backup is written first in the round
        rule.waitUntil(60_000) { file.exists() }
    }

    @Test
    fun check() {
        assumeTrue(step == "check")
        rule.waitUntil(30_000) { rule.onAllNodes(hasTestTag("lists")).fetchSemanticsNodes().isNotEmpty() }
        rule.onNodeWithTag("lists").performScrollToNode(hasTestTag("list-Kept"))
        check(rule.onAllNodes(hasTestTag("restoredInfo")).fetchSemanticsNodes().isNotEmpty()) { "restored at first launch" }
    }
}
