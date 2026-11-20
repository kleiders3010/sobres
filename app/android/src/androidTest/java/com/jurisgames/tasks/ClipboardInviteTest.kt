package com.jurisgames.tasks

import android.content.ClipData
import android.content.ClipboardManager
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.createEmptyComposeRule
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File

/** Install from an invite: the first launch finds the link the join page copied, once. */
@RunWith(AndroidJUnit4::class)
class ClipboardInviteTest {
    @get:Rule
    val rule = createEmptyComposeRule()

    private val ctx = InstrumentationRegistry.getInstrumentation().targetContext
    private val link = "https://apps.jurisgames.com/join/#abc-def-ghi-jkl-mno-pqr-stu-vwx"

    private fun copy(text: String) = InstrumentationRegistry.getInstrumentation().runOnMainSync {
        ctx.getSystemService(ClipboardManager::class.java).setPrimaryClip(ClipData.newPlainText("", text))
    }

    private fun joinDialogShown(): Boolean =
        rule.onAllNodes(hasTestTag("joinWords") and hasText(link, substring = true)).fetchSemanticsNodes().isNotEmpty()

    @Test
    fun firstLaunchTakesTheInviteOnce() {
        File(ctx.noBackupFilesDir, "clipboard_checked").delete()
        copy(link)
        ActivityScenario.launch(MainActivity::class.java).use {
            rule.waitUntil(15_000) { joinDialogShown() }
        }
        // Not on later launches.
        copy(link)
        ActivityScenario.launch(MainActivity::class.java).use {
            rule.waitForIdle()
            Thread.sleep(1500)
            check(!joinDialogShown()) { "only the first launch looks" }
        }
    }

    @Test
    fun otherClipboardTextIsLeftAlone() {
        File(ctx.noBackupFilesDir, "clipboard_checked").delete()
        copy("https://example.com/join/#not-ours")
        ActivityScenario.launch(MainActivity::class.java).use {
            rule.waitForIdle()
            Thread.sleep(1500)
            check(rule.onAllNodes(hasTestTag("joinWords")).fetchSemanticsNodes().isEmpty())
        }
    }
}
