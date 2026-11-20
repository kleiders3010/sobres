package com.jurisgames.tasks.desktop

import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.createComposeRule
import com.jurisgames.tasks.core.KtNode
import com.jurisgames.tasks.core.NodeOptions
import com.jurisgames.tasks.core.Platform
import com.jurisgames.tasks.ui.ES
import com.jurisgames.tasks.ui.Lang
import com.jurisgames.tasks.ui.Repeat
import com.jurisgames.tasks.ui.TasksApp
import com.jurisgames.tasks.ui.dayTitle
import com.jurisgames.tasks.ui.whenLabel
import org.junit.After
import org.junit.Rule
import org.junit.Test
import java.io.File
import java.nio.file.Files
import java.time.LocalDate
import java.util.Locale

/**
 * Spanish (Latin American) when the device is in Spanish, English otherwise: every text the app shows
 * through tr() has its Spanish, dates use Spanish day and month names, and the app opens in Spanish.
 */
class DesktopSpanishTest {
    @get:Rule
    val rule = createComposeRule()

    @After
    fun back() { Lang.spanish = false }

    @Test
    fun deviceLanguageDecides() {
        val was = Locale.getDefault()
        val prop = System.clearProperty("kt.lang")
        try {
            Locale.setDefault(Locale.forLanguageTag("es-AR"))
            check(Lang.pick()) { "a phone in Spanish gets Spanish" } // pinned: chosen by the device language
            Locale.setDefault(Locale.forLanguageTag("es-ES"))
            check(Lang.pick()) { "any Spanish" }
            Locale.setDefault(Locale.forLanguageTag("pt-BR"))
            check(!Lang.pick()) { "anything else stays English" }
        } finally {
            Locale.setDefault(was)
            prop?.let { System.setProperty("kt.lang", it) }
        }
    }

    /** A text wrapped in tr("...") anywhere in the app without its Spanish would show in English. */
    @Test
    fun everyTextHasItsSpanish() {
        val roots = listOf("../shared/src", "../android/src/main/java", "src/main/kotlin").map(::File)
        val lit = Regex("""\btr\("((?:[^"\\]|\\.)*)"""")
        val missing = mutableListOf<String>()
        var seen = 0
        for (root in roots) root.walkTopDown().filter { it.extension == "kt" && it.name != "Strings.kt" }.forEach { f ->
            for (m in lit.findAll(f.readText())) {
                seen++
                val key = m.groupValues[1].replace("\\\"", "\"")
                if (key !in ES && !key.contains("$")) missing += "${f.name}: $key"
            }
        }
        check(seen > 150) { "found only $seen texts: is the scan looking in the right folders?" }
        check(missing.isEmpty()) { "no Spanish for:\n" + missing.joinToString("\n") }
        check(ES.values.none { it.isBlank() })
    }

    @Test
    fun datesAndRepeatsInSpanish() {
        Lang.spanish = true
        val today = LocalDate.now()
        check(whenLabel(today.atTime(17, 0), today.atTime(9, 0)).startsWith(ES["Today"]!!))
        check(dayTitle(today, today) == ES["Today"] && dayTitle(today.plusDays(1), today) == ES["Tomorrow"]) { "Upcoming's headings" }
        val later = today.plusDays(10)
        val title = dayTitle(later, today)
        val month = later.month.getDisplayName(java.time.format.TextStyle.SHORT, Locale.forLanguageTag("es-419")).replace(".", "")
        check(title.contains(month) && !title.contains(".")) { "Spanish month, no dots: $title" }
        check(Repeat.label("2w") == ES["Every {n} weeks"]!!.replace("{n}", "2"))
        check(Repeat.label("1d") == ES["Every day"])
        check(Repeat.label("wd") == ES["Weekdays"])
        Lang.spanish = false
        check(Repeat.label("1d") != ES["Every day"]) { "English stays English" }
    }

    @Test
    fun opensInSpanish() {
        Lang.spanish = true
        val home = Files.createTempDirectory("kt-es").toFile()
        val options = NodeOptions(Platform.DESKTOP, listOf("ws://127.0.0.1:9"), false, false, "127.0.0.1:0", "", "")
        val n = KtNode.open(File(home, "identity").path, File(home, "data").path, System.getProperty("kt.worker"), options)
        rule.setContent { TasksApp(n, decodeImage = Images::decode, onPickImage = { }) }
        rule.waitUntil(5_000) { rule.onAllNodes(hasTestTag("title") and hasText(ES["My tasks"]!!), useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty() }
        check(rule.onAllNodes(hasText(ES["Add a task"]!!), useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty()) { "the add field speaks Spanish" }
        check(rule.onAllNodes(hasText("Add a task"), useUnmergedTree = true).fetchSemanticsNodes().isEmpty()) { "no English left on the first screen" }
    }
}
