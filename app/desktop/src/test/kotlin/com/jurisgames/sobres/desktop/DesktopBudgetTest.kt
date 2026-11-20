package com.jurisgames.sobres.desktop

import androidx.compose.ui.graphics.toAwtImage
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.captureToImage
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextClearance
import androidx.compose.ui.test.performTextInput
import com.jurisgames.sobres.core.KtNode
import com.jurisgames.sobres.core.NodeOptions
import com.jurisgames.sobres.core.PartItem
import com.jurisgames.sobres.core.Platform
import com.jurisgames.sobres.core.TxEdit
import com.jurisgames.sobres.ui.SobresApp
import com.jurisgames.sobres.ui.money
import com.jurisgames.sobres.ui.parseCents
import com.jurisgames.sobres.ui.today
import org.junit.After
import org.junit.Rule
import org.junit.Test
import java.io.File
import java.nio.file.Files

/**
 * The budget screens (docs/UI-SPEC.md) driven like a person would: first launch asks what is in the
 * bank, Fill gives the money jobs, "Spent…" takes it out of an envelope, Undo puts it back, a split
 * must add up, Activity and Month show it. Numbers are checked against the engine, never against
 * amounts typed into this test alone. No network.
 */
class DesktopBudgetTest {
    @get:Rule
    val rule = createComposeRule()

    private val nodes = mutableListOf<KtNode>()

    @After
    fun close() = nodes.forEach { runCatching { it.close() } }

    private fun node(): KtNode {
        val home = Files.createTempDirectory("kt-budget").toFile()
        val options = NodeOptions(Platform.DESKTOP, listOf("ws://127.0.0.1:9"), false, false, "127.0.0.1:0", "", "")
        return KtNode.open(File(home, "identity").path, File(home, "data").path, System.getProperty("kt.worker"), options).also { nodes += it }
    }

    private fun shown(tag: String) = rule.onAllNodes(hasTestTag(tag), useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty()
    private fun waitFor(tag: String) = rule.waitUntil(5_000) { shown(tag) }
    private fun gone(tag: String) = rule.waitUntil(5_000) { !shown(tag) }

    private val out = System.getProperty("kt.shots")?.takeIf { it.isNotBlank() }?.let { File(it).apply { mkdirs() } }
    private fun shot(name: String) {
        val o = out ?: return
        rule.waitForIdle()
        // A dialog is a second root: draw it centered over the dimmed screen, as a person sees it.
        val roots = rule.onAllNodes(androidx.compose.ui.test.isRoot())
        val count = roots.fetchSemanticsNodes().size
        val base = roots[0].captureToImage().toAwtImage()
        val img = java.awt.image.BufferedImage(base.getWidth(null), base.getHeight(null), java.awt.image.BufferedImage.TYPE_INT_RGB)
        val g = img.createGraphics()
        g.drawImage(base, 0, 0, null)
        for (i in 1 until count) {
            val top = roots[i].captureToImage().toAwtImage()
            g.color = java.awt.Color(0, 0, 0, 90)
            g.fillRect(0, 0, img.width, img.height)
            g.drawImage(top, (img.width - top.getWidth(null)) / 2, (img.height - top.getHeight(null)) / 2, null)
        }
        g.dispose()
        javax.imageio.ImageIO.write(img, "png", File(o, "budget-$name.png"))
    }

    @Test
    fun amountsAreReadTheWayPeopleTypeThem() {
        // pinned: amount input accepts either decimal mark and grouping (UI-SPEC "Add": amount first)
        check(parseCents("12") == 1200L)
        check(parseCents("12.5") == 1250L)
        check(parseCents("12,50") == 1250L)
        check(parseCents("1,250.00") == 125000L)
        check(parseCents("1.250,00") == 125000L)
        check(parseCents("$ 7") == 700L)
        check(parseCents("-3.10") == -310L)
        check(parseCents("") == null && parseCents("abc") == null && parseCents("1-2") == null)
        check(money(-1245, "$").startsWith("−$")) { money(-1245, "$") }
    }

    @Test
    fun firstLaunchFillSpendUndo() {
        val n = node()
        n.setDisplayName("Alex")
        rule.setContent { SobresApp(n) }
        waitFor("firstStart")
        check(n.isPristine()) { "nothing made before Start, so a backup can still be restored" }
        rule.onNodeWithTag("firstBalance").performTextInput("1000")
        rule.onNodeWithTag("firstStart").performClick()
        waitFor("available")
        val list = n.lists().single()
        var b = n.budget(list.id, today())
        check(b.accounts.single().balance == 100_000L && b.available == 100_000L) { b.toString() }
        check(b.envelopes.isNotEmpty() && b.envelopes.all { it.balance == 0L })
        check(shown("fillHint")) { "a new budget says what to do first" }
        shot("1-first")

        // Fill: "Monthly amount" fills every envelope by its monthly share; Left in Available follows.
        rule.onNodeWithTag("fillOpen").performClick()
        waitFor("fillMonthly")
        rule.onNodeWithTag("fillMonthly").performClick()
        shot("2-fill")
        rule.onNodeWithTag("fillSave").performClick()
        gone("fillSave")
        b = n.budget(list.id, today())
        val need = b.envelopes.sumOf { it.monthlyShare }
        check(b.envelopes.all { it.balance == it.monthlyShare }) { b.envelopes.toString() }
        check(b.available == 100_000L - need) { "${b.available} vs ${100_000L - need}" }
        check(!shown("fillHint"))

        // Spent: amount, what for, one envelope; it comes out of that envelope and the account.
        val groceries = b.envelopes.first()
        rule.onNodeWithTag("addSpent").performClick()
        waitFor("amount")
        rule.onNodeWithTag("addSave").assertIsNotEnabled()
        rule.onNodeWithTag("amount").performTextInput("12.50")
        rule.onNodeWithTag("payee").performTextInput("Market")
        rule.onNodeWithTag("envChip-" + groceries.name).performClick()
        shot("3-add")
        rule.onNodeWithTag("addSave").performClick()
        gone("addSave")
        b = n.budget(list.id, today())
        check(b.envelopes.first { it.id == groceries.id }.balance == groceries.balance - 1250) { b.envelopes.toString() }
        check(b.accounts.single().balance == 100_000L - 1250)
        check(b.available == 100_000L - need) { "spending from an envelope leaves Available as it was" }
        check(n.budgetActivity(list.id).first().by == "Alex") { "who entered it is kept" }

        // Undo on the snackbar takes it back out.
        rule.waitUntil(5_000) { rule.onAllNodes(hasText("Undo")).fetchSemanticsNodes().isNotEmpty() }
        rule.onNodeWithText("Undo").performClick()
        rule.waitUntil(5_000) { n.budgetActivity(list.id).none { it.kind == "exp" } }
        check(n.budget(list.id, today()).accounts.single().balance == 100_000L)

        // Again, as a split over two envelopes: Save waits until the parts add up.
        val other = b.envelopes[1]
        rule.onNodeWithTag("addSpent").performClick()
        waitFor("amount")
        rule.onNodeWithTag("amount").performTextInput("30")
        rule.onNodeWithTag("payee").performTextInput("Market")
        rule.onNodeWithTag("split").performClick()
        rule.onNodeWithTag("splitAmt-" + groceries.name).performTextInput("20")
        rule.onNodeWithTag("addSave").assertIsNotEnabled()
        rule.onNodeWithTag("splitAmt-" + other.name).performTextInput("10")
        rule.onNodeWithTag("addSave").performClick()
        gone("addSave")
        val tx = n.budgetActivity(list.id).first()
        check(tx.kind == "exp" && tx.amount == 3000L && tx.parts.map { it.cents }.sorted() == listOf(1000L, 2000L)) { tx.toString() }

        // The payee is remembered: picking it again picks its envelope and account.
        check(n.payees(list.id).first().payee == "Market")

        // Activity lists it; tapping it opens it for editing; Month shows the spending.
        rule.onNodeWithTag("secActivity").performClick()
        waitFor("tx-Market")
        shot("4-activity")
        rule.onNodeWithTag("tx-Market").performClick()
        waitFor("addDelete")
        rule.onNodeWithTag("note").performTextInput("weekly")
        rule.onNodeWithTag("addSave").performClick()
        gone("addSave")
        check(n.budgetActivity(list.id).first().note == "weekly")
        check(n.budgetActivity(list.id).size == 2) { "editing changes the entry, it doesn't add one" }
        rule.onNodeWithTag("secMonth").performClick()
        waitFor("spend-" + groceries.name)
        shot("5-month")
        rule.onNodeWithTag("secAccounts").performClick()
        waitFor("acctTotal")
        shot("6-accounts")
    }

    @Test
    fun envelopesMovesAndAccounts() {
        val n = node()
        val list = n.createList(null, "Home")
        val bank = n.setAccount(list.id, null, "Bank", "bank", 50_000)
        val food = n.setEnvelope(list.id, null, "Food", 40_000, "m", null, "Daily")
        val trip = n.setEnvelope(list.id, null, "Trip", 120_000, "g", java.time.LocalDate.now().plusMonths(12).toString(), "Saving")
        n.setTx(list.id, null, TxEdit("fill", today(), 30_000, "", "", "", "", listOf(PartItem(food, "", 30_000))))
        rule.setContent { SobresApp(n) }
        waitFor("env-Food")
        check(shown("env-Trip"))

        // Move money from Food to Trip from the envelope itself.
        rule.onNodeWithTag("env-Food").performClick()
        waitFor("envMove")
        check(shown("envPace")) { "monthly envelopes say whether they are on track" }
        rule.onNodeWithTag("envMove").performClick()
        waitFor("amount")
        rule.onNodeWithTag("amount").performTextInput("50")
        rule.onNodeWithTag("to-Trip").performClick()
        rule.onNodeWithTag("addSave").performClick()
        gone("addSave")
        var b = n.budget(list.id, today())
        check(b.envelopes.first { it.id == food }.balance == 25_000L && b.envelopes.first { it.id == trip }.balance == 5_000L) { b.envelopes.toString() }
        check(b.available == 20_000L) { "a move between envelopes leaves Available alone" }

        // Delete an envelope: its money goes back to Available, and Undo brings it back.
        rule.onNodeWithTag("env-Trip").performClick()
        waitFor("envDelete")
        rule.onNodeWithTag("envDelete").performClick()
        rule.waitUntil(5_000) { n.budget(list.id, today()).envelopes.none { it.id == trip } }
        check(n.budget(list.id, today()).available == 25_000L)
        rule.onNodeWithText("Undo").performClick()
        rule.waitUntil(5_000) { n.budget(list.id, today()).envelopes.any { it.id == trip } }

        // A new envelope from the list.
        rule.onNodeWithTag("newEnvelope").performClick()
        waitFor("envName")
        rule.onNodeWithTag("envName").performTextInput("Gifts")
        rule.onNodeWithTag("envKind-y").performClick()
        rule.onNodeWithTag("envAmount").performTextInput("600")
        rule.onNodeWithTag("envSave").performClick()
        rule.waitUntil(5_000) { shown("env-Gifts") }
        b = n.budget(list.id, today())
        val gifts = b.envelopes.first { it.name == "Gifts" }
        check(gifts.kind == "y" && gifts.amount == 60_000L && gifts.monthlyShare == 5_000L) { gifts.toString() }

        // Accounts: changing what is in it today moves Available by the same amount.
        rule.onNodeWithTag("secAccounts").performClick()
        waitFor("acct-Bank")
        rule.onNodeWithTag("acct-Bank").performClick()
        waitFor("acctBalance")
        rule.onNodeWithTag("acctBalance").performTextClearance()
        rule.onNodeWithTag("acctBalance").performTextInput("600")
        rule.onNodeWithTag("acctSave").performClick()
        gone("acctSave")
        b = n.budget(list.id, today())
        check(b.accounts.single { it.id == bank }.balance == 60_000L && b.available == 30_000L) { b.toString() }
    }

    @Test
    fun spanish() {
        com.jurisgames.sobres.ui.Lang.spanish = true
        try {
            val n = node()
            rule.setContent { SobresApp(n) }
            // pinned: UI-SPEC "Words" (Sobre, Disponible, Llenar) and the first-launch names
            rule.waitUntil(5_000) { rule.onAllNodes(hasTestTag("title") and hasText("Mi presupuesto"), useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty() }
            rule.onNodeWithTag("firstStart").performClick()
            waitFor("available")
            check(rule.onAllNodes(hasText("Disponible"), useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty())
            check(rule.onAllNodes(hasText("Llenar"), useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty())
            check(n.budget(n.lists().single().id, today()).accounts.single().name == "Banco")
        } finally {
            com.jurisgames.sobres.ui.Lang.spanish = false
        }
    }
}
