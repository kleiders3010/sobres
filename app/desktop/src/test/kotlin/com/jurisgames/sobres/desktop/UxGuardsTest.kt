package com.jurisgames.sobres.desktop

import java.io.File
import org.junit.Test

/** Source guards for the cold UX review rules (APP-RULES 136-146). */
class UxGuardsTest {
    /** APP-RULES 137: what an exception says reaches the screen only through userError (the pattern is built, rule 130). */
    @Test
    fun errorsReachTheScreenOnlyThroughUserError() {
        val raw = Regex("\\b(e|it|err)\\." + "mess" + "age\\b")
        val roots = listOf("../shared/src", "../android/src/main/java").map(::File)
        val found = mutableListOf<String>()
        var files = 0
        for (root in roots) root.walkTopDown().filter { it.extension == "kt" && it.name !in setOf("Strings.kt", "kt_ffi.kt", "Measure.kt") }.forEach { f ->
            files++
            // A log line (stderr, Android log) is not the screen.
            f.readLines().forEachIndexed { i, line -> if (raw.containsMatchIn(line) && "System.err" !in line && "Log." !in line) found += "${f.name}:${i + 1}: ${line.trim()}" }
        }
        check(files > 15) { "scanned only $files files: is the scan looking in the right folders?" }
        check(found.isEmpty()) { "raw exception text on screen:\n" + found.joinToString("\n") }
    }

    /**
     * Safety audit 2026-11-24 S4-1: the budget menu offered "Undo someone's changes…", but history.rs undo_by only
     * walks the task tree, while a budget keeps envelopes, accounts and entries in the "env" / "acct" / "tx" maps
     * (budget.rs): it always said "Nothing to undo", which in a money dispute reads as "they changed nothing".
     * The menu item may come back only once undo knows the budget maps.
     */
    @Test
    fun budgetOffersNoUndoThatCannotUndoBudgetEntries() {
        val budget = File("../shared/src/com/jurisgames/sobres/ui/Budget.kt")
        val history = File("../../crates/kt-tasks/src/history.rs")
        check(budget.isFile && history.isFile) { "moved? ${budget.absolutePath} / ${history.absolutePath}" }
        val offered = "menuUndo" in budget.readText()
        val knowsBudget = Regex("\"tx\"").containsMatchIn(history.readText()) // pinned: budget.rs entries map name
        check(!offered || knowsBudget) { "Budget.kt offers undo (menuUndo) but history.rs undo_by never touches the budget's \"tx\" entries" }
    }
}
