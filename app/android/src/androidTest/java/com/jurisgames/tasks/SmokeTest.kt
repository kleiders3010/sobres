package com.jurisgames.tasks

import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollToNode
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performTextInput
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Headless emulator smoke test: the whole stack (Kotlin UI → UniFFI → Rust engine). */
@RunWith(AndroidJUnit4::class)
class SmokeTest {
    @get:Rule
    val rule = createAndroidComposeRule<MainActivity>()

    @Test
    fun createAListAndATask() {
        val name = "Groceries" + System.currentTimeMillis() % 10000
        // Lists pile up across test runs: scroll to what's needed instead of assuming it's on screen.
        rule.onNodeWithTag("lists").performScrollToNode(hasTestTag("newlist-Personal"))
        rule.onNodeWithTag("newlist-Personal").performTextInput(name)
        rule.onNodeWithTag("addlist-Personal").performClick()
        rule.onNodeWithTag("lists").performScrollToNode(hasTestTag("list-$name"))
        rule.onNodeWithTag("list-$name").performClick()
        rule.onNodeWithTag("newtask").performTextInput("milk")
        rule.onNodeWithTag("addtask").performClick()
        rule.waitUntil(5_000) { rule.onAllNodesWithTagExists("task-milk") }
        // Small screens: the keyboard may cover the new task, so check it exists and scroll to it.
        rule.onNodeWithTag("task-milk").assertExists()
        rule.onNodeWithTag("done-milk").performScrollTo().performClick()
        // iroh's DNS resolver and network monitor can reach Android (kt-ffi src/android.rs).
        check(com.jurisgames.tasks.core.androidContextReady()) { "Android context registered" }
    }
}

private fun androidx.compose.ui.test.junit4.AndroidComposeTestRule<*, *>.onAllNodesWithTagExists(tag: String): Boolean =
    onAllNodes(androidx.compose.ui.test.hasTestTag(tag)).fetchSemanticsNodes().isNotEmpty()
