package com.jurisgames.tasks

import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.junit4.createEmptyComposeRule
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File

/** App launch (DESIGN §8): the splash waits for a sync round; blocked background sync is explained once. */
@RunWith(AndroidJUnit4::class)
class LaunchTest {
    @get:Rule
    val rule = createEmptyComposeRule()

    private val ctx = InstrumentationRegistry.getInstrumentation().targetContext

    private fun shell(cmd: String) {
        InstrumentationRegistry.getInstrumentation().uiAutomation.executeShellCommand(cmd).use { fd ->
            android.os.ParcelFileDescriptor.AutoCloseInputStream(fd).use { it.readBytes() }
        }
    }

    @Test
    fun opensAfterTheFirstRoundOrShortly() {
        val started = System.currentTimeMillis()
        ActivityScenario.launch(MainActivity::class.java).use {
            rule.waitUntil(10_000) { rule.onAllNodes(hasTestTag("sync")).fetchSemanticsNodes().isNotEmpty() }
            val node = (ctx.applicationContext as TasksApplication).node
            val waited = System.currentTimeMillis() - started
            check(node.firstRoundDone() || waited >= 1400) { "drawn after $waited ms without a round" }
        }
    }

    @Test
    fun restrictedBackgroundIsExplainedOnce() {
        val marker = File(ctx.noBackupFilesDir, "background_explained")
        marker.delete()
        shell("cmd appops set ${ctx.packageName} RUN_ANY_IN_BACKGROUND ignore")
        try {
            check(BackgroundCheck.blocked(ctx)) { "restriction not detected" }
            ActivityScenario.launch(MainActivity::class.java).use {
                rule.waitUntil(10_000) { rule.onAllNodes(hasTestTag("bgSettings")).fetchSemanticsNodes().isNotEmpty() }
            }
            ActivityScenario.launch(MainActivity::class.java).use {
                rule.waitUntil(10_000) { rule.onAllNodes(hasTestTag("sync")).fetchSemanticsNodes().isNotEmpty() }
                check(rule.onAllNodes(hasTestTag("bgSettings")).fetchSemanticsNodes().isEmpty()) { "explained only once" }
            }
        } finally {
            shell("cmd appops set ${ctx.packageName} RUN_ANY_IN_BACKGROUND allow")
        }
        check(!BackgroundCheck.blocked(ctx) || android.os.SystemClock.elapsedRealtime() > 24 * 3600 * 1000L)
    }
}
