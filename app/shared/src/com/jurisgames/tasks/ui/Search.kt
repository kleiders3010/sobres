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
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.jurisgames.tasks.core.KtNode
import com.jurisgames.tasks.core.ListItem
import com.jurisgames.tasks.core.TaskItem
import java.text.Normalizer

// Search, like Microsoft To Do's (support page 'Sort and search in lists': one search field that
// checks the word or phrase in all your tasks, steps and notes, lists each match, and selecting a
// result opens that task). Here: every list, titles, steps (subtasks) and notes; capitals and
// accents don't matter (so "cafe" finds "Café"); open tasks first, then completed ones.

/** A task that matched, the list it's in, its parent's title when it's a step, and a piece of the notes when they matched. */
data class Found(val list: ListItem, val task: TaskItem, val parent: String?, val inNotes: String?)

/** Lower case without accents, for matching. */
fun fold(s: String): String =
    Normalizer.normalize(s.lowercase(), Normalizer.Form.NFD).replace(Regex("\\p{M}+"), "")

/** Every task whose title or notes hold every word of [query], open ones first, each in list order. */
fun searchTasks(node: KtNode, lists: List<ListItem>, query: String): List<Found> {
    val words = fold(query).split(Regex("\\s+")).filter { it.isNotEmpty() }
    if (words.isEmpty()) return emptyList()
    val out = mutableListOf<Found>()
    for (l in lists) {
        val tasks = runCatching { node.tasks(l.id) }.getOrDefault(emptyList())
        val titles = tasks.associate { it.id to it.title }
        for (t in tasks) {
            val title = fold(t.title)
            val notes = fold(t.notes)
            if (words.all { it in title || it in notes }) {
                val inNotes = if (words.all { it in title }) null else snippet(t.notes, words.first { it !in title })
                out += Found(l, t, t.parentId?.let(titles::get), inNotes)
            }
        }
    }
    return out.sortedBy { it.task.done } // stable: list order stays inside each half
}

/** The line of [notes] where [word] is, cut around it. */
fun snippet(notes: String, word: String): String {
    val line = notes.lines().firstOrNull { word in fold(it) } ?: notes
    val at = fold(line).indexOf(word).coerceAtLeast(0)
    val from = (at - 30).coerceAtLeast(0)
    val to = (at + word.length + 50).coerceAtMost(line.length)
    return (if (from > 0) "…" else "") + line.substring(from, to).trim() + (if (to < line.length) "…" else "")
}

/** One field that searches every list; tapping a result opens it in its list. */
@Composable
fun SearchPane(env: Env, lists: List<ListItem>, onMenu: (() -> Unit)?, onOpen: (ListItem, String) -> Unit, chips: (@Composable () -> Unit)? = null) {
    val node = env.node
    var query by remember { mutableStateOf("") }
    val found = remember(node, env.version, query) { searchTasks(node, lists, query) }
    val c = MaterialTheme.colorScheme
    val focus = remember { FocusRequester() }
    LaunchedEffect(Unit) { runCatching { focus.requestFocus() } }
    Column(Modifier.fillMaxSize()) {
        Row(Modifier.fillMaxWidth().padding(start = 4.dp, end = 4.dp, top = 8.dp).heightIn(min = 48.dp), verticalAlignment = Alignment.CenterVertically) {
            if (onMenu != null) IconButton(onClick = onMenu, modifier = Modifier.testTag("menu")) { Icon(Ic.Menu, tr("Menu")) } else Spacer(Modifier.width(16.dp))
            Text(tr("Search"), style = MaterialTheme.typography.headlineSmall, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f).testTag("title"))
        }
        chips?.let { Spacer(Modifier.size(4.dp)); it() }
        OutlinedTextField(
            query, { query = it },
            placeholder = { Text(tr("Search all your lists")) },
            leadingIcon = { Icon(Ic.Search, null) },
            trailingIcon = if (query.isEmpty()) null else ({ IconButton({ query = "" }, Modifier.testTag("clearSearch")) { Icon(Ic.Close, tr("Clear")) } }),
            singleLine = true,
            keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp).focusRequester(focus).testTag("searchField"),
        )
        LazyColumn(Modifier.weight(1f).testTag("searchResults")) {
            if (query.isNotBlank() && found.isEmpty()) item(key = "empty") {
                Text(tr("Nothing matches “{q}”", "q" to query.trim()), color = c.onSurfaceVariant, modifier = Modifier.fillMaxWidth().padding(24.dp).testTag("empty"))
            }
            val firstDone = found.indexOfFirst { it.task.done }
            found.forEachIndexed { i, f ->
                if (i == firstDone) item(key = "completedHeader") {
                    Text(
                        tr("Completed ({n})", "n" to found.size - firstDone), style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.SemiBold,
                        modifier = Modifier.fillMaxWidth().padding(start = 20.dp, top = 16.dp, bottom = 4.dp).testTag("completedResults"),
                    )
                }
                item(key = f.list.id + "/" + f.task.id) { FoundRow(env, f, onOpen) }
            }
        }
        HorizontalDivider()
    }
}

@Composable
private fun FoundRow(env: Env, f: Found, onOpen: (ListItem, String) -> Unit) {
    val (l, t) = f
    val c = MaterialTheme.colorScheme
    Row(
        Modifier.fillMaxWidth().clickable { onOpen(l, t.id) }.padding(start = 4.dp, end = 12.dp).heightIn(min = 52.dp).testTag("found-" + t.title),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Tick(t.done, "done-" + t.title) { tickTask(env.node, l.id, t, !t.done); env.changed() }
        Column(Modifier.weight(1f).padding(vertical = 8.dp)) {
            Text(t.title, maxLines = 2, overflow = TextOverflow.Ellipsis, color = if (t.done) c.onSurfaceVariant else c.onSurface)
            f.inNotes?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = c.onSurfaceVariant, maxLines = 2, overflow = TextOverflow.Ellipsis, modifier = Modifier.testTag("inNotes")) }
            Row(verticalAlignment = Alignment.CenterVertically) {
                ListBadge(l, Modifier.padding(end = 6.dp))
                Text(
                    l.name + (f.parent?.let { "  ·  $it" } ?: "") + (t.remindMs?.let { "  ·  " + (if (t.repeat != null) "↻ " else "") + whenLabel(fromMillis(it.toLong())) } ?: ""),
                    style = MaterialTheme.typography.labelMedium, color = c.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis,
                )
            }
        }
    }
}
