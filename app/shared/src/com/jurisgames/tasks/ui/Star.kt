package com.jurisgames.tasks.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
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
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.ui.draw.clip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.jurisgames.tasks.core.KtNode
import com.jurisgames.tasks.core.ListItem
import com.jurisgames.tasks.core.TaskItem
import java.io.File

// Starred tasks and list sorting, like Google Tasks (help 7675629: Sort by My order / Date /
// Starred recently; the 2022 star: star a task, see every starred task in one place).

/** The star's colour: amber, as in Google Tasks, whatever the list's colour. */
val StarColor = Color(0xFFF2A600)

/** A star to tap; filled when the task is starred. */
@Composable
fun StarButton(starred: Boolean, tag: String, onClick: () -> Unit) {
    Box(Modifier.size(44.dp).clip(CircleShape).clickable(onClick = onClick).testTag(tag), contentAlignment = Alignment.Center) {
        Icon(
            if (starred) Ic.Star else Ic.StarBorder, if (starred) tr("Starred") else tr("Star"),
            tint = if (starred) StarColor else MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.6f),
            modifier = Modifier.size(22.dp),
        )
    }
}

fun setStar(node: KtNode, listId: String, t: TaskItem, on: Boolean) {
    runCatching { node.setStar(listId, t.id, on) }
}

/** How a list's open tasks are ordered (a choice of this device, per list). */
enum class Sort(val key: String, val label: String) {
    Order("order", "My order"),
    Date("date", "Date"),
    Starred("starred", "Starred recently"),
}

private fun sortFile(node: KtNode) = File(node.dataDir(), "ui-sort")

fun sortOf(node: KtNode, listId: String): Sort {
    val line = runCatching { sortFile(node).readLines() }.getOrDefault(emptyList()).lastOrNull { it.startsWith("$listId=") }
    return Sort.entries.find { it.key == line?.substringAfter('=') } ?: Sort.Order
}

fun setSortOf(node: KtNode, listId: String, sort: Sort) {
    val f = sortFile(node)
    val rest = runCatching { f.readLines() }.getOrDefault(emptyList()).filter { !it.startsWith("$listId=") }
    runCatching { f.writeText((rest + if (sort == Sort.Order) emptyList() else listOf("$listId=${sort.key}")).joinToString("\n")) }
}

/**
 * Top-level tasks in the chosen order (steps stay under their task). Date: the soonest reminder
 * first, tasks with none after them in your order. Starred recently: starred tasks first, the
 * newest star on top, the rest in your order.
 */
fun sortTop(top: List<TaskItem>, sort: Sort): List<TaskItem> = when (sort) {
    Sort.Order -> top
    Sort.Date -> top.sortedWith(compareBy(nullsLast()) { it.remindMs })
    Sort.Starred -> top.sortedWith(compareBy(nullsLast()) { it.starredMs?.let { ms -> -ms.toLong() } })
}

/** A starred task and the list it's in. */
data class StarredTask(val list: ListItem, val task: TaskItem)

/** Open starred tasks (steps too) of every list, the newest star first. */
fun starredTasks(node: KtNode, lists: List<ListItem>): List<StarredTask> =
    lists.flatMap { l -> runCatching { node.tasks(l.id) }.getOrDefault(emptyList()).filter { it.starredMs != null && !it.done }.map { StarredTask(l, it) } }
        .sortedByDescending { it.task.starredMs?.toLong() ?: 0L }

/** Every starred task in one place. Tapping one opens it in its list. */
@Composable
fun StarredPane(env: Env, lists: List<ListItem>, onMenu: (() -> Unit)?, onOpen: (ListItem, String) -> Unit, chips: (@Composable () -> Unit)? = null) {
    val node = env.node
    val starred = remember(node, env.version) { starredTasks(node, lists) }
    val c = MaterialTheme.colorScheme
    Column(Modifier.fillMaxSize()) {
        Row(Modifier.fillMaxWidth().padding(start = 4.dp, end = 4.dp, top = 8.dp).heightIn(min = 48.dp), verticalAlignment = Alignment.CenterVertically) {
            if (onMenu != null) IconButton(onClick = onMenu, modifier = Modifier.testTag("menu")) { Icon(Ic.Menu, tr("Menu")) } else Spacer(Modifier.width(16.dp))
            Text(tr("Starred"), style = MaterialTheme.typography.headlineSmall, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f).testTag("title"))
        }
        chips?.let { Spacer(Modifier.size(4.dp)); it() }
        LazyColumn(Modifier.weight(1f).testTag("starredTasks")) {
            if (starred.isEmpty()) item(key = "empty") {
                Text(tr("Nothing starred"), color = c.onSurfaceVariant, modifier = Modifier.fillMaxWidth().padding(24.dp).testTag("empty"))
            }
            items(starred, key = { it.list.id + "/" + it.task.id }) { (l, t) ->
                Row(
                    Modifier.fillMaxWidth().clickable { onOpen(l, t.id) }.padding(start = 4.dp, end = 12.dp).heightIn(min = 52.dp).testTag("starred-" + t.title),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Tick(false, "done-" + t.title) { tickTask(node, l.id, t, true); env.changed() }
                    Column(Modifier.weight(1f).padding(vertical = 8.dp)) {
                        Text(t.title, maxLines = 2, overflow = TextOverflow.Ellipsis)
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            ListBadge(l, Modifier.padding(end = 6.dp))
                            Text(
                                l.name + (t.remindMs?.let { "  ·  " + (if (t.repeat != null) "↻ " else "") + whenLabel(fromMillis(it.toLong())) } ?: ""),
                                style = MaterialTheme.typography.labelMedium, color = c.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis,
                            )
                        }
                    }
                    StarButton(true, "star-" + t.title) { setStar(node, l.id, t, false); env.changed() }
                }
            }
        }
        HorizontalDivider()
    }
}
