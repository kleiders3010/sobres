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
    // -Pstore: Google Play size (1080x1920 at a phone's 2.625 density) instead of the 412x860 review size
    private val store = System.getProperty("kt.shots.store") == "true"
    // sample names in the language the shots are taken in (-Plang=es for the Spanish store listing)
    private val es = System.getProperty("kt.lang") == "es"
    private fun t(en: String, sp: String) = if (es) sp else en
    private val out = System.getProperty("kt.shots")?.takeIf { it.isNotBlank() }?.let { File(it).apply { mkdirs() } }

    @Test
    fun phone() = flow(if (store) 1080 else 412, if (store) 1920 else 860, fit = false)

    /** APP-RULES 196: the same screens on a small phone (360 dp), every fixed button wholly on screen (PhoneFit.kt). */
    @Test
    fun everyButtonFitsASmallPhone() { if (!store) flow(360, 760, fit = true) }

    private fun flow(w: Int, h: Int, fit: Boolean) = runDesktopComposeUiTest(width = w, height = h) {
        val problems = mutableListOf<String>()
        val home = Files.createTempDirectory("kt-phone").toFile()
        val n = KtNode.open(File(home, "identity").path, File(home, "data").path, System.getProperty("kt.worker"),
            NodeOptions(Platform.DESKTOP, listOf("ws://127.0.0.1:9"), false, false, "127.0.0.1:0", "", ""))
        val l = n.createList(null, t("Home", "Casa"))
        n.setListStyle(l.id, budgetHue(0), 1u, null)
        n.setCurrency(l.id, "$")
        val bank = n.setAccount(l.id, null, t("Bank", "Banco"), "bank", 2_480_00)
        n.setAccount(l.id, null, t("Cash", "Efectivo"), "cash", 60_00)
        val envs = listOf(
            Triple(t("Groceries", "Supermercado"), 400_00L, t("Food", "Comida")), Triple(t("Eating out", "Salidas"), 80_00L, t("Food", "Comida")),
            Triple(t("Rent", "Alquiler"), 900_00L, t("Bills", "Cuentas")), Triple(t("Phone", "Celular"), 25_00L, t("Bills", "Cuentas")), Triple(t("Fun", "Gustos"), 60_00L, ""),
        ).map { (name, amt, g) -> n.setEnvelope(l.id, null, name, amt, "m", null, g) }
        val gifts = n.setEnvelope(l.id, null, t("Gifts", "Regalos"), 600_00, "y", null, t("Saving", "Ahorro"))
        n.setEnvelope(l.id, null, t("Trip", "Viaje"), 1_000_00, "g", java.time.LocalDate.now().plusMonths(8).withDayOfMonth(1).toString(), t("Saving", "Ahorro"))
        val plan = n.fillPlan(l.id, "add", today())
        n.setTx(l.id, null, TxEdit("fill", today(), plan.sumOf { it.cents }, "", "", "", "", plan))
        n.setTx(l.id, null, TxEdit("exp", today(), 37_50, bank, "", t("Market", "Verdulería"), "", listOf(PartItem(envs[0], "", 37_50))))
        n.setTx(l.id, null, TxEdit("exp", today(), 37_45, bank, "", t("Phone bill", "Factura del celular"), "", listOf(PartItem(envs[3], "", 37_45))))
        n.setTx(l.id, null, TxEdit("exp", today(), 22_00, bank, "", t("Pizza", "Pizza"), "", listOf(PartItem(envs[1], "", 22_00))))
        check(gifts.isNotEmpty())
        java.io.File(n.dataDir(), "ui-last-list").writeText(l.id)
        setContent {
            if (store) androidx.compose.runtime.CompositionLocalProvider(androidx.compose.ui.platform.LocalDensity provides androidx.compose.ui.unit.Density(2.625f)) { SobresApp(n) }
            else SobresApp(n)
        }
        waitUntil(timeoutMillis = 5_000) { onAllNodes(hasTestTag("env-" + t("Groceries", "Supermercado")), useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty() }
        check(onAllNodes(hasTestTag("addSpent")).fetchSemanticsNodes().isNotEmpty()) { "the Spent bar is there on a phone" }
        fun shot(name: String) {
            // APP-RULE 225: a store picture is fit-checked too, so a broken label never goes public
            if (fit || store) problems += buttonsFit(w, name, OPEN_LISTS[name] ?: 0)
            if (fit) return
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
            javax.imageio.ImageIO.write(img, "png", File(o, (if (store) "store-" else "phone-") + "$name.png"))
        }
        shot("1-envelopes")
        onNodeWithTag("env-" + t("Groceries", "Supermercado")).performClick()
        waitUntil(timeoutMillis = 5_000) { onAllNodes(hasTestTag("envPace"), useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty() }
        shot("2-envelope")
        onNodeWithTag("envDone").performClick()
        onNodeWithTag("addSpent").performClick()
        waitUntil(timeoutMillis = 5_000) { onAllNodes(hasTestTag("amount"), useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty() }
        shot("3-spent")
        onNodeWithTag("kind-fill").performClick()
        shot("4-move")
        n.close()
        check(problems.isEmpty()) { problems.joinToString("\n") }
    }

    // sideways-scrolling rows each screen may have (open-ended lists only, APP-RULES 196)
    private val OPEN_LISTS = mapOf<String, Int>()
}
