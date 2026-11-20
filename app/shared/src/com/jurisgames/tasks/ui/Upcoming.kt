package com.jurisgames.tasks.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.jurisgames.tasks.core.KtNode
import com.jurisgames.tasks.core.ListItem
import java.time.LocalDate
import java.time.LocalDateTime
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import java.util.Locale

// Upcoming, like Todoist's Today and Upcoming views (help 'Plan your day with the Today view',
// 'Plan your week with the Upcoming view'): every open task with a date, from every list, under
// its day; tasks from earlier days under Overdue with one button to move them all to today. Tasks
// with no date aren't here (as in Todoist). A repeating task shows its next date only.

/** One day's heading and its tasks, soonest first. `overdue` is the group of earlier days. */
data class DayGroup(val title: String, val overdue: Boolean, val tasks: List<StarredTask>)

/** Open tasks with a date, from every list. */
fun datedTasks(node: KtNode, lists: List<ListItem>): List<StarredTask> =
    lists.flatMap { l -> runCatching { node.tasks(l.id) }.getOrDefault(emptyList()).filter { it.remindMs != null && !it.done }.map { StarredTask(l, it) } }

/** Overdue (earlier days), then Today, Tomorrow and one heading per later day; each soonest first. */
fun dayGroups(tasks: List<StarredTask>, now: LocalDateTime = LocalDateTime.now()): List<DayGroup> {
    val today = now.toLocalDate()
    val sorted = tasks.sortedBy { it.task.remindMs!!.toLong() }
    val (late, rest) = sorted.partition { fromMillis(it.task.remindMs!!.toLong()).toLocalDate() < today }
    val out = mutableListOf<DayGroup>()
    if (late.isNotEmpty()) out += DayGroup(tr("Overdue"), true, late)
    rest.groupBy { fromMillis(it.task.remindMs!!.toLong()).toLocalDate() }.forEach { (day, ts) -> out += DayGroup(dayTitle(day, today), false, ts) }
    return out
}

/** "Today", "Tomorrow", then "Fri 21 Nov" (with the year when it isn't this year). */
fun dayTitle(day: LocalDate, today: LocalDate): String = when (day) {
    today -> tr("Today")
    today.plusDays(1) -> tr("Tomorrow")
    else -> day.format(DateTimeFormatter.ofPattern(if (day.year == today.year) "EEE d MMM" else "EEE d MMM yyyy", Lang.locale)).let(::shortName)
}

/** Every overdue task to today, each keeping its time of day (Todoist's Reschedule, set to today). */
fun moveToToday(node: KtNode, overdue: List<StarredTask>, now: LocalDateTime = LocalDateTime.now()) {
    for ((l, t) in overdue) {
        val at = fromMillis(t.remindMs!!.toLong())
        runCatching { node.setReminder(l.id, t.id, now.toLocalDate().atTime(at.toLocalTime()).millis().toULong()) }
    }
}

/** Every dated task in one place, by day. Tapping one opens it in its list. */
@Composable
fun UpcomingPane(env: Env, lists: List<ListItem>, onMenu: (() -> Unit)?, onOpen: (ListItem, String) -> Unit, chips: (@Composable () -> Unit)? = null) {
    val node = env.node
    val groups = remember(node, env.version) { dayGroups(datedTasks(node, lists)) }
    val c = MaterialTheme.colorScheme
    Column(Modifier.fillMaxSize()) {
        Row(Modifier.fillMaxWidth().padding(start = 4.dp, end = 4.dp, top = 8.dp).heightIn(min = 48.dp), verticalAlignment = Alignment.CenterVertically) {
            if (onMenu != null) IconButton(onClick = onMenu, modifier = Modifier.testTag("menu")) { Icon(Ic.Menu, tr("Menu")) } else Spacer(Modifier.width(16.dp))
            Text(tr("Upcoming"), style = MaterialTheme.typography.headlineSmall, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f).testTag("title"))
        }
        chips?.let { Spacer(Modifier.size(4.dp)); it() }
        LazyColumn(Modifier.weight(1f).testTag("upcomingTasks")) {
            if (groups.isEmpty()) item(key = "empty") {
                Text(tr("Nothing with a date"), color = c.onSurfaceVariant, modifier = Modifier.fillMaxWidth().padding(24.dp).testTag("empty"))
            }
            for (g in groups) {
                item(key = "day-" + g.title) {
                    Row(Modifier.fillMaxWidth().padding(start = 20.dp, end = 8.dp, top = 12.dp).heightIn(min = 40.dp).testTag("day-" + g.title), verticalAlignment = Alignment.CenterVertically) {
                        Text(
                            g.title, style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.SemiBold,
                            color = if (g.overdue) c.error else c.onSurface, modifier = Modifier.weight(1f),
                        )
                        if (g.overdue) TextButton(onClick = { moveToToday(node, g.tasks); env.changed() }, modifier = Modifier.testTag("moveToToday")) { Text(tr("Move to today")) }
                    }
                }
                items(g.tasks, key = { it.list.id + "/" + it.task.id }) { (l, t) ->
                    val at = fromMillis(t.remindMs!!.toLong())
                    val time = if (g.overdue) whenLabel(at) else at.toLocalTime().format(DateTimeFormatter.ofLocalizedTime(FormatStyle.SHORT))
                    Row(
                        Modifier.fillMaxWidth().clickable { onOpen(l, t.id) }.padding(start = 4.dp, end = 12.dp).heightIn(min = 52.dp).testTag("upcoming-" + t.title),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Tick(false, "done-" + t.title) { tickTask(node, l.id, t, true); env.changed() }
                        Column(Modifier.weight(1f).padding(vertical = 8.dp)) {
                            Text(t.title, maxLines = 2, overflow = TextOverflow.Ellipsis)
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                ListBadge(l, Modifier.padding(end = 6.dp))
                                Text(
                                    l.name + "  ·  " + (if (t.repeat != null) "↻ " else "") + time,
                                    style = MaterialTheme.typography.labelMedium, color = if (g.overdue) c.error else c.onSurfaceVariant,
                                    maxLines = 1, overflow = TextOverflow.Ellipsis,
                                )
                            }
                        }
                        if (l.listType != "shopping") StarButton(t.starredMs != null, "star-" + t.title) { setStar(node, l.id, t, t.starredMs == null); env.changed() }
                    }
                }
            }
        }
        HorizontalDivider()
    }
}
