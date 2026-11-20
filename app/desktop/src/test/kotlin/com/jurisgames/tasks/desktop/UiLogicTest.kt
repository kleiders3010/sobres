package com.jurisgames.tasks.desktop

import com.jurisgames.tasks.core.TaskItem
import com.jurisgames.tasks.ui.Repeat
import com.jurisgames.tasks.ui.parseWhen
import com.jurisgames.tasks.ui.sections
import com.jurisgames.tasks.ui.settleWord
import com.jurisgames.tasks.ui.suggestions
import java.time.LocalDateTime
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull

/**
 * The UI's pure rules: when-words, list sections, shopping suggestions, code word boxes.
 * pinned: every expected value here is the meaning decided in docs/UI-SPEC.md and UX-NOTES.md
 * ("tomorrow at 5" = 17:00, a day alone = 9:00, plain numbers are not times, 4th letter settles a word).
 */
class UiLogicTest {
    // A Wednesday afternoon.
    private val now = LocalDateTime.of(2026, 11, 18, 14, 0)

    @Test
    fun whenWords() {
        val a = parseWhen("call mum tomorrow at 5", now)!!
        assertEquals(LocalDateTime.of(2026, 11, 19, 17, 0), a.at)
        assertEquals("call mum", a.title)
        assertEquals(LocalDateTime.of(2026, 11, 20, 9, 0), parseWhen("pay rent friday", now)!!.at)
        assertEquals(LocalDateTime.of(2026, 11, 18, 17, 30), parseWhen("gym 17:30", now)!!.at)
        assertEquals(LocalDateTime.of(2026, 11, 19, 9, 0), parseWhen("dentist at 9am", now)!!.at) // 9am already passed today
        assertEquals(LocalDateTime.of(2026, 11, 18, 16, 0), parseWhen("water plants in 2 hours", now)!!.at)
        assertEquals(LocalDateTime.of(2026, 11, 19, 9, 0), parseWhen("comprar pan mañana", now)!!.at)
        assertEquals("comprar pan", parseWhen("comprar pan mañana", now)!!.title)
        assertEquals(LocalDateTime.of(2026, 11, 23, 9, 0), parseWhen("plan trip next week", now)!!.at)
        // Plain numbers are not times.
        assertNull(parseWhen("buy 5 eggs", now))
        assertNull(parseWhen("read chapter 12", now))
        assertNull(parseWhen("milk", now))
    }

    // pinned: Todoist's "every" words and rule meanings (Repeat.kt), English and Spanish
    @Test
    fun repeatWords() {
        fun p(s: String) = parseWhen(s, now)!!.let { Triple(it.title, it.repeat, it.at) }
        assertEquals(Triple("bins out", "1w", LocalDateTime.of(2026, 11, 23, 9, 0)), p("bins out every monday"))
        assertEquals(Triple("standup", "wd", LocalDateTime.of(2026, 11, 19, 9, 30)), p("standup every weekday at 9:30"))
        assertEquals(Triple("pay rent", "1m", LocalDateTime.of(2026, 11, 19, 9, 0)), p("pay rent every month"))
        assertEquals("2d", p("water plants every 2 days").second)
        assertEquals(Triple("gym", "2w", LocalDateTime.of(2026, 11, 19, 9, 0)), p("every other week gym"))
        assertEquals(Triple("vitamins", "1d", LocalDateTime.of(2026, 11, 19, 9, 0)), p("daily vitamins"))
        assertEquals(Triple("call grandma", "1w", LocalDateTime.of(2026, 11, 22, 18, 0)), p("call grandma every sunday at 6"))
        assertEquals(Triple("sacar la basura", "1w", LocalDateTime.of(2026, 11, 23, 9, 0)), p("sacar la basura todos los lunes"))
        assertEquals(Triple("revisar cuentas", "2w", LocalDateTime.of(2026, 11, 19, 9, 0)), p("revisar cuentas cada 2 semanas"))
        assertEquals("1d", p("regar todos los días").second)
        assertNull(parseWhen("buy every flavour", now))
        assertNull(parseWhen("call mum tomorrow", now)!!.repeat)
    }

    @Test
    fun repeatNextDates() {
        val mon = LocalDateTime.of(2026, 11, 23, 9, 0)
        assertEquals(LocalDateTime.of(2026, 11, 30, 9, 0), Repeat.next("1w", mon, mon.plusHours(1)))
        // ticked late: counts from when it was due, and never lands in the past
        assertEquals(LocalDateTime.of(2026, 11, 19, 9, 0), Repeat.next("1d", LocalDateTime.of(2026, 11, 10, 9, 0), now))
        assertEquals(LocalDateTime.of(2026, 11, 24, 9, 0), Repeat.next("1w", LocalDateTime.of(2026, 11, 17, 9, 0), now))
        // ticked early: still one step on
        assertEquals(LocalDateTime.of(2026, 11, 30, 9, 0), Repeat.next("1w", mon, now))
        val fri = LocalDateTime.of(2026, 11, 20, 9, 0)
        assertEquals(LocalDateTime.of(2026, 11, 23, 9, 0), Repeat.next("wd", fri, fri.plusHours(1)))
        assertEquals(LocalDateTime.of(2027, 2, 28, 9, 0), Repeat.next("1m", LocalDateTime.of(2027, 1, 31, 9, 0), LocalDateTime.of(2027, 2, 1, 0, 0)))
        assertEquals(LocalDateTime.of(2027, 11, 23, 9, 0), Repeat.next("1y", mon, mon))
        assertEquals("Every 2 weeks", Repeat.label("2w"))
        for (r in Repeat.CHOICES) check(Repeat.valid(r)) { r }
        check(!Repeat.valid("0d") && !Repeat.valid("weekly"))
    }

    private fun task(id: String, done: Boolean = false, parent: String? = null, title: String = id) =
        TaskItem(id, parent, if (parent == null) 0u else 1u, title, done, "", false, emptyList(), null, null, null, null, null, false)

    @Test
    fun completedFoldsAway() {
        val ts = listOf(task("a"), task("a1", parent = "a", done = true), task("b", done = true), task("b1", parent = "b"), task("c"))
        val s = sections(ts, emptySet())
        assertEquals(listOf("a", "a1", "c"), s.active.map { it.id })
        assertEquals(listOf("b", "b1"), s.completed.map { it.id })
        // Just ticked: stays in place for a moment.
        assertEquals(listOf("a", "a1", "b", "b1", "c"), sections(ts, setOf("b")).active.map { it.id })
        // Deleted with Undo pending: hidden.
        assertEquals(listOf("c"), sections(ts, emptySet(), setOf("a")).active.map { it.id })
    }

    @Test
    fun shoppingSuggestsFinishedItems() {
        val ts = listOf(task("1", true, title = "Milk"), task("2", true, title = "Oat milk"), task("3", false, title = "Milkshake"), task("4", true, title = "milk"))
        assertEquals(listOf("Milk", "Oat milk"), suggestions(ts, "mil").map { it.title })
        assertEquals(emptyList(), suggestions(ts, " "))
    }

    @Test
    fun wordBoxes() {
        val words = listOf("apple", "ant", "bake", "banjo")
        val complete = { p: String -> words.singleOrNull { it.startsWith(p) } }
        assertNull(settleWord("ap", complete))
        assertNull(settleWord("app", complete)) // 3 letters: wait for the 4th to catch a typo
        assertEquals("apple", settleWord("appl", complete))
        assertEquals("", settleWord("apxl", complete)) // typo
        assertEquals("ant", settleWord("ant", complete)) // a whole short word
        assertEquals("", settleWord("zzz", complete))
    }
}
