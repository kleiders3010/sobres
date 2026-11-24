package com.jurisgames.sobres.desktop

import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.assertHeightIsAtLeast
import androidx.compose.ui.test.assertIsSelected
import androidx.compose.ui.test.assertWidthIsAtLeast
import androidx.compose.ui.unit.dp
import com.jurisgames.sobres.ui.hueName
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import com.jurisgames.sobres.core.KtNode
import com.jurisgames.sobres.core.NodeOptions
import com.jurisgames.sobres.core.PersonItem
import com.jurisgames.sobres.core.Platform
import com.jurisgames.sobres.core.accentColor
import com.jurisgames.sobres.ui.SobresApp
import com.jurisgames.sobres.ui.personLabel
import org.junit.Rule
import org.junit.Test
import java.io.File
import java.nio.file.Files

/** Appearance (DESIGN §2): list color and icon in the list's data, themes per device, avatar tags. */
class DesktopAppearanceTest {
    @get:Rule
    val rule = createComposeRule()

    private fun shown(tag: String) = rule.onAllNodes(hasTestTag(tag), useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty()

    @Test
    fun listStyleAndTheme() {
        val home = Files.createTempDirectory("kt-look").toFile()
        val options = NodeOptions(Platform.DESKTOP, listOf("ws://127.0.0.1:9"), false, false, "127.0.0.1:0", "", "")
        val n = KtNode.open(File(home, "identity").path, File(home, "data").path, System.getProperty("kt.worker"), options)
        n.createList(null, "Groceries")
        rule.setContent { SobresApp(n) }
        rule.waitUntil(5_000) { shown("list-Groceries") }
        check(shown("dot-Groceries") && !shown("icon-Groceries")) { "grey dot, no icon yet" }
        rule.onNodeWithTag("list-Groceries").performClick()
        rule.onNodeWithTag("listMenu").performClick()
        rule.onNodeWithTag("styleOpen").performClick()
        // APP-RULES 140/209 guard: each swatch is a named radio button with a 48 dp target (they were bare 32 dp dots)
        rule.onNodeWithTag("hue-130").assert(SemanticsMatcher.expectValue(SemanticsProperties.ContentDescription, listOf(hueName(130.0))))
            .assert(SemanticsMatcher.expectValue(SemanticsProperties.Role, Role.RadioButton))
            .assertWidthIsAtLeast(48.dp).assertHeightIsAtLeast(48.dp)
        rule.onNodeWithTag("hue-130").performClick()
        rule.onNodeWithTag("hue-130").assertIsSelected()
        rule.onNodeWithTag("vivid-2").performClick()
        rule.onNodeWithTag("pick-cart").performClick()
        rule.onNodeWithTag("styleSave").performClick()
        rule.waitUntil(5_000) { shown("icon-Groceries") }
        val l = n.lists().single { it.name == "Groceries" }
        check(l.hue == 130.0 && l.vivid.toInt() == 2 && l.icon == "cart") { l.toString() }

        // Themes are per device and decide the tone: same hue, different color.
        val before = accentColor("default", l.hue, l.vivid, false)
        check(before != accentColor("default", l.hue, l.vivid, true)) { "dark mode has its own tone" }
        rule.onNodeWithTag("settingsOpen").performClick()
        rule.waitUntil(5_000) { shown("theme-ocean") }
        check(!shown("set-dynamic")) { "no wallpaper colors on desktop" }
        rule.onNodeWithTag("theme-ocean").performClick()
        rule.onNodeWithTag("settingsSave").performClick()
        rule.waitUntil(5_000) { !shown("settingsSave") }
        check(n.settings().theme == "ocean")
        n.close()
    }

    @Test
    fun sameNamesGetATag() {
        val a = PersonItem("Alex", false, "A", 10.0, "4F2A")
        val b = PersonItem("Alex", false, "A", 200.0, "91C0")
        val c = PersonItem("Sam", true, "S", 40.0, "0B11")
        val all = listOf(a, b, c)
        check(personLabel(a, all) == "Alex · 4F2A" && personLabel(b, all) == "Alex · 91C0")
        check(personLabel(c, all) == "Sam (you)")
    }
}
