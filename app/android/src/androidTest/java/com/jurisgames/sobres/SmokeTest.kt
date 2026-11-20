package com.jurisgames.sobres

import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.jurisgames.sobres.ui.today
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Headless emulator smoke test: the whole stack (Kotlin UI → UniFFI → Rust engine). */
@RunWith(AndroidJUnit4::class)
class SmokeTest {
    @get:Rule
    val rule = createAndroidComposeRule<MainActivity>()

    @Test
    fun recordASpend() {
        val node = (rule.activity.application as TasksApplication).node
        // Budgets pile up across test runs: make one with its own name and open it from the engine side.
        val name = "Home" + System.currentTimeMillis() % 10000
        val list = node.createList(null, name)
        node.setAccount(list.id, null, "Bank", "bank", 10_000)
        val food = node.setEnvelope(list.id, null, "Food", 5_000, "m", null, "")
        java.io.File(node.dataDir(), "ui-last-list").writeText(list.id)
        rule.activityRule.scenario.recreate()
        rule.waitUntil(10_000) { rule.onAllNodes(androidx.compose.ui.test.hasTestTag("env-Food")).fetchSemanticsNodes().isNotEmpty() }
        rule.onNodeWithTag("addSpent").performClick()
        rule.waitUntil(5_000) { rule.onAllNodes(androidx.compose.ui.test.hasTestTag("amount")).fetchSemanticsNodes().isNotEmpty() }
        rule.onNodeWithTag("amount").performTextInput("12.50")
        rule.onNodeWithTag("envChip-Food").performClick()
        rule.onNodeWithTag("addSave").performClick()
        rule.waitUntil(5_000) { node.budget(list.id, today()).envelopes.single { it.id == food }.balance == -1_250L }
        // iroh's DNS resolver and network monitor can reach Android (kt-ffi src/android.rs).
        check(com.jurisgames.sobres.core.androidContextReady()) { "Android context registered" }
    }
}
