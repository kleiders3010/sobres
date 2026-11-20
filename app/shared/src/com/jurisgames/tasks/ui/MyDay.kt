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
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.jurisgames.tasks.core.KtNode
import com.jurisgames.tasks.core.ListItem
import com.jurisgames.tasks.core.TaskItem
import kotlinx.coroutines.delay
import java.time.LocalDate
import java.time.LocalDateTime
import java.time.format.DateTimeFormatter

// My Day, like Microsoft To Do's (help 'My Day and suggestions'): pick what you'll do today from
// any list; it starts empty every day; what wasn't finished comes back as a suggestion, with
// tasks due soon and starred ones. Each person's own: a shared list's members each have theirs.

/** The sun of My Day, amber like Microsoft To Do's. */
val SunColor = Color(0xFFE8A200)

/** Today, as My Day stores it. */
fun dayKey(day: LocalDate = LocalDate.now()): String = day.toString()

fun inMyDay(t: TaskItem, today: String = dayKey()): Boolean = t.myDay == today

fun setMyDay(node: KtNode, listId: String, t: TaskItem, on: Boolean, today: String = dayKey()) {
    runCatching { node.setMyDay(listId, t.id, if (on) today else null) }
}

/** Today's tasks of every list: open ones in list order, then the ones done. */
fun myDayTasks(node: KtNode, lists: List<ListItem>, today: String = dayKey()): List<StarredTask> =
    lists.flatMap { l -> runCatching { node.tasks(l.id) }.getOrDefault(emptyList()).filter { it.myDay == today }.map { StarredTask(l, it) } }
        .sortedBy { it.task.done }

/** A suggestions section. */
enum class Idea(val label: String) { Yesterday("Yesterday"), Earlier("Earlier"), Soon("Due soon"), Starred("Starred") }

/**
 * Suggestions for today, each task once, in this order: left in My Day yesterday, left there
 * earlier, a reminder that's late, today or tomorrow (soonest first), starred (newest first).
 * Open tasks only, none already in today.
 */
fun dayIdeas(node: KtNode, lists: List<ListItem>, now: LocalDateTime = LocalDateTime.now()): List<Pair<Idea, StarredTask>> {
    val today = now.toLocalDate()
    val (key, yesterday) = dayKey(today) to dayKey(today.minusDays(1))
    val soon = today.plusDays(2).atStartOfDay()
    val open = lists.flatMap { l -> runCatching { node.tasks(l.id) }.getOrDefault(emptyList()).filter { !it.done && it.myDay != key }.map { StarredTask(l, it) } }
    val out = mutableListOf<Pair<Idea, StarredTask>>()
    val seen = mutableSetOf<String>()
    fun take(idea: Idea, xs: List<StarredTask>) = xs.forEach { if (seen.add(it.list.id + "/" + it.task.id)) out += idea to it }
    take(Idea.Yesterday, open.filter { it.task.myDay == yesterday })
    take(Idea.Earlier, open.filter { it.task.myDay != null && it.task.myDay!! < yesterday }.sortedByDescending { it.task.myDay })
    take(Idea.Soon, open.filter { it.task.remindMs?.let { ms -> fromMillis(ms.toLong()).isBefore(soon) } == true }.sortedBy { it.task.remindMs?.toLong() })
    take(Idea.Starred, open.filter { it.task.starredMs != null }.sortedByDescending { it.task.starredMs?.toLong() })
    return out
}

/** "Thursday, November 19" / "jueves, 19 de noviembre". */
fun dayTitle(day: LocalDate): String =
    day.format(DateTimeFormatter.ofPattern(if (Lang.spanish) "EEEE, d 'de' MMMM" else "EEEE, MMMM d", Lang.locale)).replaceFirstChar { it.uppercase(Lang.locale) }

/** My Day: today's tasks from every list, the suggestions, and an add field (adds to [addTo]). */
@Composable
fun MyDayPane(env: Env, lists: List<ListItem>, addTo: ListItem?, onMenu: (() -> Unit)?, onOpen: (ListItem, String) -> Unit, chips: (@Composable () -> Unit)? = null) {
    val node = env.node
    var today by remember { mutableStateOf(LocalDate.now()) }
    LaunchedEffect(Unit) { while (true) { delay(30_000); today = LocalDate.now() } } // a new day starts empty
    val key = dayKey(today)
    val tasks = remember(node, env.version, key) { myDayTasks(node, lists, key) }
    val ideas = remember(node, env.version, key) { dayIdeas(node, lists, LocalDateTime.now()) }
    var showIdeas by remember { mutableStateOf(false) }
    val c = MaterialTheme.colorScheme
    Column(Modifier.fillMaxSize()) {
        Row(Modifier.fillMaxWidth().padding(start = 4.dp, end = 4.dp, top = 8.dp).heightIn(min = 48.dp), verticalAlignment = Alignment.CenterVertically) {
            if (onMenu != null) IconButton(onClick = onMenu, modifier = Modifier.testTag("menu")) { Icon(Ic.Menu, tr("Menu")) } else Spacer(Modifier.width(16.dp))
            Column(Modifier.weight(1f)) {
                Text(tr("My Day"), style = MaterialTheme.typography.headlineSmall, fontWeight = FontWeight.SemiBold, modifier = Modifier.testTag("title"))
                Text(dayTitle(today), style = MaterialTheme.typography.labelLarge, color = c.onSurfaceVariant, modifier = Modifier.testTag("dayTitle"))
            }
            IconButton(onClick = { showIdeas = !showIdeas }, modifier = Modifier.testTag("dayIdeas")) {
                Icon(Ic.Lightbulb, tr("Suggestions"), tint = if (showIdeas) SunColor else c.onSurfaceVariant)
            }
        }
        chips?.let { Spacer(Modifier.size(4.dp)); it() }
        LazyColumn(Modifier.weight(1f).testTag("myDayTasks")) {
            if (showIdeas) {
                item(key = "ideasHeader") {
                    Row(Modifier.fillMaxWidth().padding(start = 20.dp, end = 8.dp, top = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                        Text(tr("Suggestions"), style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
                        TextButton(onClick = { showIdeas = false }, modifier = Modifier.testTag("ideasDone")) { Text(tr("Done")) }
                    }
                }
                if (ideas.isEmpty()) item(key = "noIdeas") {
                    Text(tr("No suggestions right now"), color = c.onSurfaceVariant, modifier = Modifier.fillMaxWidth().padding(24.dp).testTag("noIdeas"))
                }
                var last: Idea? = null
                for ((idea, st) in ideas) {
                    if (idea != last) {
                        last = idea
                        item(key = "idea-" + idea.name) { SectionHeading(tr(idea.label), "ideas-" + idea.name) }
                    }
                    item(key = "i/" + st.list.id + "/" + st.task.id) {
                        DayRow(env, st, onOpen) {
                            IconButton({ setMyDay(node, st.list.id, st.task, true, key); env.changed() }, Modifier.testTag("addDay-" + st.task.title)) {
                                Icon(Ic.Add, tr("Add to My Day"), tint = c.primary)
                            }
                        }
                    }
                }
            } else {
                if (tasks.isEmpty()) item(key = "empty") {
                    Column(Modifier.fillMaxWidth().padding(24.dp)) {
                        Text(tr("Nothing in your day yet"), color = c.onSurfaceVariant, modifier = Modifier.testTag("empty"))
                        if (ideas.isNotEmpty()) TextButton(onClick = { showIdeas = true }, modifier = Modifier.testTag("seeIdeas")) {
                            Text(tr("See suggestions ({n})", "n" to ideas.size))
                        }
                    }
                }
                val firstDone = tasks.indexOfFirst { it.task.done }
                tasks.forEachIndexed { i, st ->
                    if (i == firstDone) item(key = "completedHeader") { SectionHeading(tr("Completed ({n})", "n" to tasks.size - firstDone), "completedDay") }
                    item(key = st.list.id + "/" + st.task.id) {
                        DayRow(env, st, onOpen) {
                            IconButton({ setMyDay(node, st.list.id, st.task, false); env.changed() }, Modifier.testTag("unday-" + st.task.title)) {
                                Icon(Ic.Close, tr("Remove from My Day"), tint = c.onSurfaceVariant)
                            }
                        }
                    }
                }
            }
        }
        if (addTo != null) {
            val focus = remember { FocusRequester() }
            var text by remember { mutableStateOf("") }
            var ignoreWhen by remember { mutableStateOf(false) }
            val parsed = if (ignoreWhen) null else parseWhen(text)
            fun add() {
                val raw = text.trim()
                if (raw.isEmpty()) return
                val id = runCatching { node.addTask(addTo.id, parsed?.title?.ifBlank { raw } ?: raw, null) }.getOrNull() ?: return
                if (parsed != null) runCatching { node.setReminder(addTo.id, id, parsed.at.millis().toULong()) }
                if (parsed?.repeat != null) runCatching { node.setRepeat(addTo.id, id, parsed.repeat) }
                runCatching { node.setMyDay(addTo.id, id, key) }
                text = ""; ignoreWhen = false
                showIdeas = false
                env.changed()
            }
            Column(Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 6.dp)) {
                AddField(text, { text = it; if (it.isEmpty()) ignoreWhen = false }, parsed, { ignoreWhen = true }, tr("Add a task to {list}", "list" to addTo.name), focus, ::add)
            }
        }
        HorizontalDivider()
    }
}

@Composable
private fun SectionHeading(text: String, tag: String) {
    Text(text, style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.SemiBold, modifier = Modifier.fillMaxWidth().padding(start = 20.dp, top = 16.dp, bottom = 4.dp).testTag(tag))
}

@Composable
private fun DayRow(env: Env, st: StarredTask, onOpen: (ListItem, String) -> Unit, end: @Composable () -> Unit) {
    val (l, t) = st
    val c = MaterialTheme.colorScheme
    Row(
        Modifier.fillMaxWidth().clickable { onOpen(l, t.id) }.padding(start = 4.dp, end = 4.dp).heightIn(min = 52.dp).testTag("day-" + t.title),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Tick(t.done, "done-" + t.title) { tickTask(env.node, l.id, t, !t.done); env.changed() }
        Column(Modifier.weight(1f).padding(vertical = 8.dp)) {
            Text(
                t.title, maxLines = 2, overflow = TextOverflow.Ellipsis, color = if (t.done) c.onSurfaceVariant else c.onSurface,
                textDecoration = if (t.done) TextDecoration.LineThrough else null,
            )
            Row(verticalAlignment = Alignment.CenterVertically) {
                ListBadge(l, Modifier.padding(end = 6.dp))
                Text(
                    l.name + (t.remindMs?.let { "  ·  " + (if (t.repeat != null) "↻ " else "") + whenLabel(fromMillis(it.toLong())) } ?: ""),
                    style = MaterialTheme.typography.labelMedium, color = c.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis,
                )
                if (t.starredMs != null) Icon(Ic.Star, tr("Starred"), tint = StarColor, modifier = Modifier.padding(start = 6.dp).size(14.dp))
            }
        }
        end()
    }
}
