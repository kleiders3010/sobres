package com.jurisgames.sobres.desktop

import androidx.compose.ui.graphics.toAwtImage
import androidx.compose.ui.test.ExperimentalTestApi
import androidx.compose.ui.test.captureToImage
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.isRoot
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.runDesktopComposeUiTest
import com.jurisgames.sobres.core.KtNode
import com.jurisgames.sobres.core.NodeOptions
import com.jurisgames.sobres.core.PartItem
import com.jurisgames.sobres.core.Platform
import com.jurisgames.sobres.core.TxEdit
import com.jurisgames.sobres.ui.SobresApp
import com.jurisgames.sobres.ui.budgetHue
import com.jurisgames.sobres.ui.today
import org.junit.Test
import java.io.File
import java.nio.file.Files

/**
 * Phone-sized pictures of a lived-in budget for a review (-Pshots=<folder>); skipped otherwise.
 * Also checks the phone layout keeps the "Spent…" bar on screen.
 */
@OptIn(ExperimentalTestApi::class)
class PhoneShotsTest {
    private val out = System.getProperty("kt.shots")?.takeIf { it.isNotBlank() }?.let { File(it).apply { mkdirs() } }

    @Test
    fun phone() = runDesktopComposeUiTest(width = 412, height = 860) {
        val home = Files.createTempDirectory("kt-phone").toFile()
        val n = KtNode.open(File(home, "identity").path, File(home, "data").path, System.getProperty("kt.worker"),
            NodeOptions(Platform.DESKTOP, listOf("ws://127.0.0.1:9"), false, false, "127.0.0.1:0", "", ""))
        val l = n.createList(null, "Home")
        n.setListStyle(l.id, budgetHue(0), 1u, null)
        n.setCurrency(l.id, "$")
        val bank = n.setAccount(l.id, null, "Bank", "bank", 2_480_00)
        n.setAccount(l.id, null, "Cash", "cash", 60_00)
        val envs = listOf(
            Triple("Groceries", 400_00L, "Food"), Triple("Eating out", 80_00L, "Food"),
            Triple("Rent", 900_00L, "Bills"), Triple("Phone", 25_00L, "Bills"), Triple("Fun", 60_00L, ""),
        ).map { (name, amt, g) -> n.setEnvelope(l.id, null, name, amt, "m", null, g) }
        val gifts = n.setEnvelope(l.id, null, "Gifts", 600_00, "y", null, "Saving")
        n.setEnvelope(l.id, null, "Trip", 1_000_00, "g", java.time.LocalDate.now().plusMonths(8).withDayOfMonth(1).toString(), "Saving")
        val plan = n.fillPlan(l.id, "add", today())
        n.setTx(l.id, null, TxEdit("fill", today(), plan.sumOf { it.cents }, "", "", "", "", plan))
        n.setTx(l.id, null, TxEdit("exp", today(), 37_50, bank, "", "Market", "", listOf(PartItem(envs[0], "", 37_50))))
        n.setTx(l.id, null, TxEdit("exp", today(), 37_45, bank, "", "Phone bill", "", listOf(PartItem(envs[3], "", 37_45))))
        n.setTx(l.id, null, TxEdit("exp", today(), 22_00, bank, "", "Pizza", "", listOf(PartItem(envs[1], "", 22_00))))
        check(gifts.isNotEmpty())
        java.io.File(n.dataDir(), "ui-last-list").writeText(l.id)
        setContent { SobresApp(n) }
        waitUntil(timeoutMillis = 5_000) { onAllNodes(hasTestTag("env-Groceries"), useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty() }
        check(onAllNodes(hasTestTag("addSpent")).fetchSemanticsNodes().isNotEmpty()) { "the Spent bar is there on a phone" }
        fun shot(name: String) {
            val o = out ?: return
            waitForIdle()
            val roots = onAllNodes(isRoot())
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
            javax.imageio.ImageIO.write(img, "png", File(o, "phone-$name.png"))
        }
        shot("1-envelopes")
        onNodeWithTag("env-Groceries").performClick()
        waitUntil(timeoutMillis = 5_000) { onAllNodes(hasTestTag("envPace"), useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty() }
        shot("2-envelope")
        onNodeWithTag("envDone").performClick()
        onNodeWithTag("addSpent").performClick()
        waitUntil(timeoutMillis = 5_000) { onAllNodes(hasTestTag("amount"), useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty() }
        shot("3-spent")
        onNodeWithTag("kind-fill").performClick()
        shot("4-move")
        n.close()
    }
}
