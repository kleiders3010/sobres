package com.jurisgames.sobres.desktop

import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import com.jurisgames.sobres.core.KtNode
import com.jurisgames.sobres.core.NodeOptions
import com.jurisgames.sobres.core.PartItem
import com.jurisgames.sobres.core.Platform
import com.jurisgames.sobres.core.TxEdit
import com.jurisgames.sobres.ui.SobresApp
import com.jurisgames.sobres.ui.today
import org.junit.After
import org.junit.Rule
import org.junit.Test
import java.io.File
import java.nio.file.Files

/**
 * A shared budget on two devices: Alex makes it and invites Sam; both record spending at the same
 * time while apart; after they sync, both see the same balances, every entry counted once, and
 * who entered each. Two nodes and a local loopback relay; nothing leaves this machine.
 */
class DesktopBudgetSyncTest {
    @get:Rule
    val rule = createComposeRule()

    private val port = 18800 + (ProcessHandle.current().pid() % 300).toInt()
    private val relay: Process = ProcessBuilder(System.getProperty("kt.cli"), "relay", "--port", port.toString()).redirectErrorStream(true).start().also {
        it.inputStream.bufferedReader().readLine()
    }
    private val nodes = mutableListOf<KtNode>()

    @After
    fun stop() {
        nodes.forEach { runCatching { it.close() } }
        relay.destroyForcibly()
    }

    private fun node(tag: String): KtNode {
        val home = Files.createTempDirectory("kt-$tag").toFile()
        val options = NodeOptions(Platform.DESKTOP, listOf("ws://127.0.0.1:$port"), false, false, "127.0.0.1:0", tag, "")
        return KtNode.open(File(home, "identity").path, File(home, "data").path, System.getProperty("kt.worker"), options).also { nodes += it }
    }

    private fun until(what: String, seconds: Int = 180, ok: () -> Boolean) {
        val deadline = System.currentTimeMillis() + seconds * 1000L
        while (!ok()) {
            check(System.currentTimeMillis() < deadline) { what }
            nodes.forEach { it.syncNow() }
        }
    }

    @Test
    fun twoPeopleSpendAtOnce() {
        val a = node("alex")
        val b = node("sam")
        a.setDisplayName("Alex")
        b.setDisplayName("Sam")
        val home = a.createList(null, "Home")
        val bank = a.setAccount(home.id, null, "Bank", "bank", 100_000)
        val food = a.setEnvelope(home.id, null, "Food", 40_000, "m", null, "")
        a.setTx(home.id, null, TxEdit("fill", today(), 40_000, "", "", "", "", listOf(PartItem(food, "", 40_000))))
        val shared = a.shareList(home.id)
        val invite = a.invite(shared.groupId, 7u)
        a.syncNow()
        b.joinCode(invite.link)
        fun bList() = b.lists().find { it.name == "Home" }
        until("Sam never got the budget") { bList()?.let { l -> runCatching { b.budget(l.id, today()).envelopes.isNotEmpty() }.getOrDefault(false) } == true }
        val bl = bList()!!

        // Both spend from Food while apart (no sync between the two entries).
        a.setTx(shared.id, null, TxEdit("exp", today(), 1_500, bank, "", "Bakery", "", listOf(PartItem(food, "", 1_500))))
        b.setTx(bl.id, null, TxEdit("exp", today(), 2_500, bank, "", "Market", "", listOf(PartItem(food, "", 2_500))))
        until("the two entries never met") { a.budgetActivity(shared.id).size == 3 && b.budgetActivity(bl.id).size == 3 }

        val ba = a.budget(shared.id, today())
        val bb = b.budget(bl.id, today())
        check(ba == bb) { "both devices show the same budget:\n$ba\n$bb" }
        val left = ba.envelopes.single().balance
        check(left == 40_000L - 1_500 - 2_500) { "every entry counted once: $left" }
        check(ba.accounts.single().balance == 100_000L - 4_000)
        check(b.budgetActivity(bl.id).map { it.by }.toSet().containsAll(setOf("Alex", "Sam"))) { "who entered each entry travels with it" }

        // Sam's screen shows the shared budget, with who entered what in Activity.
        rule.setContent { SobresApp(b) }
        rule.waitUntil(10_000) { rule.onAllNodes(hasTestTag("env-Food"), useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty() }
        rule.onNodeWithTag("secActivity").performClick()
        rule.waitUntil(5_000) { rule.onAllNodes(hasTestTag("tx-Bakery"), useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty() }
        check(rule.onAllNodes(hasText("Alex", substring = true), useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty()) { "the entry says Alex entered it" }
    }
}
