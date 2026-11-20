package com.jurisgames.tasks.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
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
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.InputChip
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
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.jurisgames.tasks.core.KtNode
import com.jurisgames.tasks.core.ListItem
import com.jurisgames.tasks.core.PersonItem
import com.jurisgames.tasks.core.TaskItem
import com.jurisgames.tasks.core.accentColor

// Assigning tasks in shared lists, like Microsoft To Do (support "Assign tasks in shared lists": in a
// shared list, a task's details have "Assign to": any member of the list, yourself too; "Assigned to
// me" gathers everything assigned to you; moving a task to another list drops its assignment).

/** The same person (a name can repeat in a list: the tag tells them apart). */
fun samePerson(a: PersonItem?, b: PersonItem?): Boolean =
    a != null && b != null && a.name == b.name && a.isMe == b.isMe && a.tag == b.tag

/** A person's initials on their colour, small enough for a task row. */
@Composable
fun SmallAvatar(p: PersonItem, size: Dp = 22.dp, modifier: Modifier = Modifier) {
    val look = LocalLook.current
    Box(modifier.size(size).background(argb(accentColor(look.themeId, p.hue, 1u.toUByte(), look.dark)), CircleShape), contentAlignment = Alignment.Center) {
        Text(p.initials, color = if (look.dark) Color.Black else Color.White, fontSize = (size.value * 0.42f).sp, maxLines = 1)
    }
}

/** The list is shared (not one of your own lists). */
fun isShared(node: KtNode, list: ListItem): Boolean =
    runCatching { node.groups() }.getOrDefault(emptyList()).find { it.id == list.groupId }?.personal == false

fun assign(node: KtNode, listId: String, t: TaskItem, p: PersonItem?) {
    runCatching { node.setAssignee(listId, t.id, p) }
}

/** "Assign to" in a task's details: the list's people as chips; tap the chosen one again for nobody. */
@OptIn(androidx.compose.foundation.layout.ExperimentalLayoutApi::class)
@Composable
fun AssignTo(env: Env, list: ListItem, t: TaskItem) {
    val node = env.node
    val people = remember(node, env.version, list.id) { runCatching { node.people(list.id) }.getOrDefault(emptyList()) }
    FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), modifier = Modifier.testTag("assignTo")) {
        val gone = t.assigned?.takeIf { a -> people.none { samePerson(it, a) } }
        if (gone != null) InputChip(
            true, { assign(node, list.id, t, null); env.changed() },
            label = { Text(gone.name) }, leadingIcon = { SmallAvatar(gone) },
            trailingIcon = { Icon(Ic.Close, tr("Not assigned"), Modifier.size(16.dp)) },
            modifier = Modifier.testTag("assignee-gone"),
        )
        for (p in people) {
            val on = samePerson(p, t.assigned)
            FilterChip(
                on, { assign(node, list.id, t, if (on) null else p); env.changed() },
                label = { Text(personLabel(p, people)) }, leadingIcon = { SmallAvatar(p) },
                trailingIcon = if (on) ({ Icon(Ic.Close, tr("Not assigned"), Modifier.size(16.dp)) }) else null,
                modifier = Modifier.testTag("assign-" + p.name),
            )
        }
    }
}

/** An assigned task and the list it's in. */
data class AssignedTask(val list: ListItem, val task: TaskItem)

/** Open tasks (steps too) assigned to you in every list: the soonest reminder first, then by list. */
fun assignedTasks(node: KtNode, lists: List<ListItem>): List<AssignedTask> =
    lists.flatMap { l -> runCatching { node.tasks(l.id) }.getOrDefault(emptyList()).filter { it.assignedToMe && !it.done }.map { AssignedTask(l, it) } }
        .sortedWith(compareBy(nullsLast()) { it.task.remindMs })

/** Everything assigned to you, from every list. Tapping one opens it in its list. */
@Composable
fun AssignedPane(env: Env, lists: List<ListItem>, onMenu: (() -> Unit)?, onOpen: (ListItem, String) -> Unit, chips: (@Composable () -> Unit)? = null) {
    val node = env.node
    val mine = remember(node, env.version) { assignedTasks(node, lists) }
    val c = MaterialTheme.colorScheme
    Column(Modifier.fillMaxSize()) {
        Row(Modifier.fillMaxWidth().padding(start = 4.dp, end = 4.dp, top = 8.dp).heightIn(min = 48.dp), verticalAlignment = Alignment.CenterVertically) {
            if (onMenu != null) IconButton(onClick = onMenu, modifier = Modifier.testTag("menu")) { Icon(Ic.Menu, tr("Menu")) } else Spacer(Modifier.width(16.dp))
            Text(tr("Assigned to me"), style = MaterialTheme.typography.headlineSmall, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f).testTag("title"))
        }
        chips?.let { Spacer(Modifier.size(4.dp)); it() }
        LazyColumn(Modifier.weight(1f).testTag("assignedTasks")) {
            if (mine.isEmpty()) item(key = "empty") {
                Text(tr("Nothing assigned to you"), color = c.onSurfaceVariant, modifier = Modifier.fillMaxWidth().padding(24.dp).testTag("empty"))
            }
            items(mine, key = { it.list.id + "/" + it.task.id }) { (l, t) ->
                Row(
                    Modifier.fillMaxWidth().clickable { onOpen(l, t.id) }.padding(start = 4.dp, end = 12.dp).heightIn(min = 52.dp).testTag("assigned-" + t.title),
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
                    if (l.listType != "shopping") StarButton(t.starredMs != null, "star-" + t.title) { setStar(node, l.id, t, t.starredMs == null); env.changed() }
                }
            }
        }
        HorizontalDivider()
    }
}
