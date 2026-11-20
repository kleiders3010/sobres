package com.jurisgames.tasks.ui

import java.time.DayOfWeek
import java.time.LocalDate
import java.time.LocalDateTime
import java.time.LocalTime
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import java.time.temporal.TemporalAdjusters

/**
 * "When" words typed into a new task ("call mum tomorrow at 5"), English and Spanish.
 * [at] is the moment meant, [title] the text without the when-words.
 */
data class When(val at: LocalDateTime, val title: String, val repeat: String? = null)

private val WEEKDAYS = mapOf(
    "monday" to DayOfWeek.MONDAY, "tuesday" to DayOfWeek.TUESDAY, "wednesday" to DayOfWeek.WEDNESDAY,
    "thursday" to DayOfWeek.THURSDAY, "friday" to DayOfWeek.FRIDAY, "saturday" to DayOfWeek.SATURDAY, "sunday" to DayOfWeek.SUNDAY,
    "lunes" to DayOfWeek.MONDAY, "martes" to DayOfWeek.TUESDAY, "miercoles" to DayOfWeek.WEDNESDAY, "miércoles" to DayOfWeek.WEDNESDAY,
    "jueves" to DayOfWeek.THURSDAY, "viernes" to DayOfWeek.FRIDAY, "sabado" to DayOfWeek.SATURDAY, "sábado" to DayOfWeek.SATURDAY,
    "domingo" to DayOfWeek.SUNDAY,
)

/** Time of day when only a day is given. */
val MORNING: LocalTime = LocalTime.of(9, 0)

private val day = Regex(
    "(?i)(?<![\\p{L}])(?:(?:on|el)\\s+)?(today|hoy|tonight|esta noche|tomorrow|mañana|manana|next week|la semana que viene|la próxima semana|la proxima semana|" +
        WEEKDAYS.keys.joinToString("|") + ")(?![\\p{L}])",
)
private val clock = Regex(
    "(?i)(?<![\\p{L}\\d])(?:(at|a las|a la)\\s+)?(\\d{1,2})(?::(\\d{2}))?\\s*(am|pm|a\\.m\\.|p\\.m\\.)?(?![\\p{L}\\d])",
)
private val relative = Regex("(?i)(?<![\\p{L}])(?:in|en)\\s+(\\d{1,3})\\s*(min|mins|minutes|minutos|h|hour|hours|hora|horas|day|days|día|dias|días)(?![\\p{L}])")

// "every monday", "every 2 weeks", "daily", "todos los lunes", "cada 2 semanas" (Todoist's "every" words, English and Spanish)
private val PLURAL_DAYS = mapOf("sábados" to DayOfWeek.SATURDAY, "sabados" to DayOfWeek.SATURDAY, "domingos" to DayOfWeek.SUNDAY)
private val every = Regex(
    "(?i)(?<![\\p{L}])(?:(?:every|cada|todos los|todas las)\\s+(other\\s+|otra\\s+)?(?:(\\d{1,2})\\s+)?" +
        "(days?|weekdays?|workdays?|weeks?|months?|years?|d[ií]as?|semanas?|mes|meses|años?|anos?|" +
        (WEEKDAYS.keys + PLURAL_DAYS.keys).joinToString("|") + ")" +
        "|(daily|weekly|monthly|yearly|a diario|de lunes a viernes|d[ií]as de semana|d[ií]as h[aá]biles))(?![\\p{L}])",
)

private fun everyRule(m: MatchResult): Pair<String, DayOfWeek?> {
    val word = m.groupValues[3].lowercase()
    val alone = m.groupValues[4].lowercase()
    if (alone.isNotEmpty()) return when (alone) {
        "daily", "a diario" -> "1d"
        "weekly" -> "1w"
        "monthly" -> "1m"
        "yearly" -> "1y"
        else -> "wd"
    } to null
    val n = when {
        m.groupValues[1].isNotEmpty() -> 2
        m.groupValues[2].isNotEmpty() -> m.groupValues[2].toInt().coerceIn(1, 99)
        else -> 1
    }
    val dow = WEEKDAYS[word] ?: PLURAL_DAYS[word]
    if (dow != null) return "${n}w" to dow
    val unit = when {
        word.startsWith("weekday") || word.startsWith("workday") -> return "wd" to null
        word.startsWith("day") || word.startsWith("d") -> "d"
        word.startsWith("week") || word.startsWith("semana") -> "w"
        word.startsWith("month") || word.startsWith("mes") -> "m"
        else -> "y"
    }
    return "$n$unit" to null
}

fun parseWhen(text: String, now: LocalDateTime = LocalDateTime.now()): When? {
    every.find(text)?.let { m ->
        val (rule, dow) = everyRule(m)
        val rest = strip(text, m.range)
        val inner = parseOnce(rest, now)
        val weekend = { d: LocalDateTime -> d.dayOfWeek == DayOfWeek.SATURDAY || d.dayOfWeek == DayOfWeek.SUNDAY }
        var at = when {
            dow != null -> {
                val time = inner?.at?.toLocalTime() ?: MORNING
                var d = now.toLocalDate().atTime(time)
                while (d.dayOfWeek != dow || !d.isAfter(now)) d = d.plusDays(1)
                d
            }
            inner != null -> inner.at
            else -> firstDate(rule, now)
        }
        while (rule == "wd" && weekend(at)) at = at.plusDays(1)
        return When(at, inner?.title ?: rest, rule)
    }
    return parseOnce(text, now)
}

private fun parseOnce(text: String, now: LocalDateTime): When? {
    relative.find(text)?.let { m ->
        val n = m.groupValues[1].toLong()
        val u = m.groupValues[2].lowercase()
        val at = when {
            u.startsWith("m") -> now.plusMinutes(n)
            u.startsWith("h") -> now.plusHours(n)
            else -> now.plusDays(n).with(MORNING)
        }
        return When(at.withSecond(0).withNano(0), strip(text, m.range))
    }
    val d = day.find(text)
    var date: LocalDate? = null
    var defaultTime = MORNING
    if (d != null) {
        val w = d.groupValues[1].lowercase()
        date = when (w) {
            "today", "hoy" -> now.toLocalDate()
            "tonight", "esta noche" -> { defaultTime = LocalTime.of(20, 0); now.toLocalDate() }
            "tomorrow", "mañana", "manana" -> now.toLocalDate().plusDays(1)
            "next week", "la semana que viene", "la próxima semana", "la proxima semana" -> now.toLocalDate().with(TemporalAdjusters.next(DayOfWeek.MONDAY))
            else -> now.toLocalDate().with(TemporalAdjusters.next(WEEKDAYS.getValue(w)))
        }
    }
    // A number is a time only with "at"/"a las", am/pm, or minutes ("buy 5 eggs" stays as it is).
    val t = clock.findAll(text).firstOrNull { m ->
        val h = m.groupValues[2].toInt()
        val min = m.groupValues[3]
        (m.groupValues[1].isNotEmpty() || m.groupValues[4].isNotEmpty() || min.isNotEmpty()) && h <= 23 && (min.isEmpty() || min.toInt() < 60) &&
            (d == null || !m.range.overlaps(d.range))
    }
    var time: LocalTime? = null
    if (t != null) {
        var h = t.groupValues[2].toInt()
        val min = t.groupValues[3].ifEmpty { "0" }.toInt()
        val ampm = t.groupValues[4].lowercase()
        if (ampm.startsWith("p") && h < 12) h += 12
        if (ampm.startsWith("a") && h == 12) h = 0
        // "at 5" without am/pm means the afternoon; tonight's "at 9" means 21:00.
        if (ampm.isEmpty() && t.groupValues[3].isEmpty() && h in 1..7) h += 12
        if (ampm.isEmpty() && defaultTime.hour >= 20 && h in 1..11) h += 12
        time = LocalTime.of(h % 24, min)
    }
    if (date == null && time == null) return null
    val at = if (date != null) {
        LocalDateTime.of(date, time ?: defaultTime)
    } else {
        val today = LocalDateTime.of(now.toLocalDate(), time!!)
        if (today.isAfter(now)) today else today.plusDays(1)
    }
    val ranges = listOfNotNull(d?.range, t?.range)
    return When(at, strip(text, *ranges.toTypedArray()))
}

private fun IntRange.overlaps(o: IntRange) = first <= o.last && o.first <= last

private fun strip(text: String, vararg ranges: IntRange): String {
    var s = text
    for (r in ranges.sortedByDescending { it.first }) s = s.removeRange(r)
    return s.replace(Regex("\\s{2,}"), " ").trim()
}

fun LocalDateTime.millis(): Long = atZone(ZoneId.systemDefault()).toInstant().toEpochMilli()

fun fromMillis(ms: Long): LocalDateTime = LocalDateTime.ofInstant(java.time.Instant.ofEpochMilli(ms), ZoneId.systemDefault())

/** "Today 17:00", "Tomorrow 9:00", "Fri 9:00", "12 Dec 9:00": short, for chips and rows. */
fun whenLabel(at: LocalDateTime, now: LocalDateTime = LocalDateTime.now()): String {
    val time = at.toLocalTime().format(DateTimeFormatter.ofLocalizedTime(FormatStyle.SHORT))
    val days = java.time.temporal.ChronoUnit.DAYS.between(now.toLocalDate(), at.toLocalDate())
    val dayText = when {
        days == 0L -> tr("Today")
        days == 1L -> tr("Tomorrow")
        days == -1L -> tr("Yesterday")
        days in 2..6 -> shortName(at.dayOfWeek.getDisplayName(java.time.format.TextStyle.SHORT, Lang.locale))
        else -> shortName(at.format(DateTimeFormatter.ofPattern("d MMM", Lang.locale)))
    }
    return "$dayText $time"
}
