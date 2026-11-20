package com.jurisgames.sobres

import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assume.assumeTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * Cross-device steps driven from the host (`am instrument -e ...`), with real networks:
 * 1. `-e words a-b-c-...`: join a group with an invite's words through the UI.
 * 2. `-e list NAME`: sync until that list shows up (the inviter's device let us in).
 */
@RunWith(AndroidJUnit4::class)
class InviteFlowTest {
    @get:Rule
    val rule = createAndroidComposeRule<MainActivity>()
    private val args = InstrumentationRegistry.getArguments()

    @Test
    fun joinWithWords() {
        val words = args.getString("words")
        assumeTrue(words != null)
        rule.onNodeWithTag("join").performClick()
        rule.onNodeWithTag("joinWords").performTextInput(words!!.replace('-', ' '))
        rule.onNodeWithTag("joinGo").performClick()
        try {
            rule.waitUntil(120_000) {
                rule.onAllNodes(hasTestTag("joinResult") and (hasText("Asked to join", substring = true) or hasText("Joined", substring = true))).fetchSemanticsNodes().isNotEmpty()
            }
        } catch (e: Throwable) {
            val shown = rule.onAllNodes(hasTestTag("joinResult")).fetchSemanticsNodes().flatMap { n ->
                n.config.getOrNull(androidx.compose.ui.semantics.SemanticsProperties.Text)?.map { it.text } ?: emptyList()
            }
            val node = (rule.activity.application as TasksApplication).node
            val relays = node.relayStats().joinToString("\n") { "${it.url} ok=${it.publishedOk} fail=${it.connectFailures} inrow=${it.failuresInARow} rb=${it.readbackHits}/${it.readbackMisses}" }
            throw AssertionError("join result shown: $shown\nrelays:\n$relays", e)
        }
    }

    @Test
    fun listArrives() {
        val list = args.getString("list")
        assumeTrue(list != null)
        val deadline = System.currentTimeMillis() + 240_000
        while (System.currentTimeMillis() < deadline) {
            if (rule.onAllNodes(hasTestTag("list-$list")).fetchSemanticsNodes().isNotEmpty()) return
            rule.onNodeWithTag("sync").performClick()
            rule.waitUntil(120_000) { rule.onAllNodes(hasTestTag("status") and hasText("Sent", substring = true)).fetchSemanticsNodes().isNotEmpty() }
            rule.waitForIdle()
            Thread.sleep(3_000)
        }
        throw AssertionError("list $list never arrived")
    }
}
