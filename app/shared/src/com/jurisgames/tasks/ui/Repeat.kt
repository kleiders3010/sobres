package com.jurisgames.tasks.ui

import com.jurisgames.tasks.core.KtNode
import com.jurisgames.tasks.core.TaskItem
import java.time.DayOfWeek
import java.time.LocalDateTime

/**
 * Repeating tasks, like Todoist's "every": ticking one off moves the same task to its next date
 * (counted from when it was due, so a late tick doesn't shift the days), with its steps unticked.
 * Rules (kt_tasks::valid_repeat): "wd" Monday to Friday, or a count and a unit: "1d", "2w", "1m", "1y".
 */
object Repeat {
    /** The choices offered in a task's details, in order (pinned: the shipped rule strings). */
    val CHOICES = listOf("1d", "wd", "1w", "2w", "1m", "1y")

    fun valid(rule: String): Boolean =
        rule == "wd" || Regex("[1-9]\\d?[dwmy]").matches(rule)

    /** One step on from [d]. */
    fun step(rule: String, d: LocalDateTime): LocalDateTime {
        if (rule == "wd") {
            var x = d.plusDays(1)
            while (x.dayOfWeek == DayOfWeek.SATURDAY || x.dayOfWeek == DayOfWeek.SUNDAY) x = x.plusDays(1)
            return x
        }
        val n = rule.dropLast(1).toLong()
        return when (rule.last()) {
            'd' -> d.plusDays(n)
            'w' -> d.plusWeeks(n)
            'm' -> d.plusMonths(n)
            else -> d.plusYears(n)
        }
    }

    /** The next date after it was due at [due]: at least one step, and never in the past. */
    fun next(rule: String, due: LocalDateTime, now: LocalDateTime = LocalDateTime.now()): LocalDateTime {
        var x = step(rule, due)
        var guard = 0
        while (!x.isAfter(now) && guard++ < 10_000) x = step(rule, x)
        return x
    }

    /** "Every day", "Weekdays", "Every 2 weeks"... */
    fun label(rule: String): String {
        if (rule == "wd") return tr("Weekdays")
        val n = rule.dropLast(1).toInt()
        val unit = when (rule.last()) { 'd' -> "day"; 'w' -> "week"; 'm' -> "month"; else -> "year" }
        return if (n == 1) tr("Every $unit") else tr("Every {n} ${unit}s", "n" to n)
    }
}

/**
 * Tick a task on or off. A repeating task being ticked off isn't done: it moves to its next date.
 * Returns that date when it moved, so the caller can say so.
 */
fun tickTask(node: KtNode, listId: String, t: TaskItem, done: Boolean, now: LocalDateTime = LocalDateTime.now()): LocalDateTime? {
    val rule = t.repeat
    if (done && rule != null && Repeat.valid(rule)) {
        val due = t.remindMs?.let { fromMillis(it.toLong()) } ?: now
        val next = Repeat.next(rule, due, now)
        if (runCatching { node.repeatDone(listId, t.id, next.millis().toULong()) }.isSuccess) return next
    }
    runCatching { node.setDone(listId, t.id, done) }
    return null
}

/** The first date for a rule just picked on a task with no reminder: the next fitting day, at 9. */
fun firstDate(rule: String, now: LocalDateTime = LocalDateTime.now()): LocalDateTime {
    val today = now.toLocalDate().atTime(MORNING)
    val weekend = { d: LocalDateTime -> d.dayOfWeek == DayOfWeek.SATURDAY || d.dayOfWeek == DayOfWeek.SUNDAY }
    var x = if (today.isAfter(now)) today else today.plusDays(1)
    while (rule == "wd" && weekend(x)) x = x.plusDays(1)
    return x
}
