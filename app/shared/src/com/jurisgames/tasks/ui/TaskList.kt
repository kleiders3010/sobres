package com.jurisgames.tasks.ui

import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.AssistChip
import androidx.compose.material3.DatePicker
import androidx.compose.material3.DatePickerDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.InputChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SnackbarDuration
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.SnackbarResult
import androidx.compose.material3.SuggestionChip
import androidx.compose.material3.SwipeToDismissBox
import androidx.compose.material3.SwipeToDismissBoxValue
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TextField
import androidx.compose.material3.TextFieldDefaults
import androidx.compose.material3.TimePicker
import androidx.compose.material3.VerticalDivider
import androidx.compose.material3.rememberDatePickerState
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.material3.rememberSwipeToDismissBoxState
import androidx.compose.material3.rememberTimePickerState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.jurisgames.tasks.core.KtNode
import com.jurisgames.tasks.core.ListItem
import com.jurisgames.tasks.core.TaskItem
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import sh.calvin.reorderable.ReorderableItem
import sh.calvin.reorderable.rememberReorderableLazyListState
import java.time.DayOfWeek
import java.time.LocalDateTime
import java.time.LocalTime
import java.time.temporal.TemporalAdjusters

/** Turns image bytes into something to draw (each platform provides its decoder). */
val LocalImageDecoder = staticCompositionLocalOf<((ByteArray) -> ImageBitmap?)?> { null }

/** How long a ticked item stays in place before moving to "Completed" (a mis-tap stays visible). */
const val SETTLE_MS = 1000L

/** The rows of a list: what's still to do (each with its subtasks), then what's completed. */
data class Sections(val active: List<TaskItem>, val completed: List<TaskItem>)

fun sections(tasks: List<TaskItem>, settling: Set<String>, hidden: Set<String> = emptySet(), sort: Sort = Sort.Order): Sections {
    val byParent = tasks.filter { it.id !in hidden }.groupBy { it.parentId }
    fun subtree(t: TaskItem): List<TaskItem> = listOf(t) + byParent[t.id].orEmpty().flatMap { subtree(it) }
    val top = byParent[null].orEmpty()
    val (active, done) = top.partition { !it.done || it.id in settling }
    return Sections(sortTop(active, sort).flatMap { subtree(it) }, done.flatMap { subtree(it) })
}

/** Finished items in a shopping list that match what's being typed, to bring back with a tap. */
fun suggestions(tasks: List<TaskItem>, typed: String, max: Int = 4): List<TaskItem> {
    val t = typed.trim().lowercase()
    if (t.isEmpty()) return emptyList()
    return tasks.filter { it.done && it.parentId == null && it.title.lowercase().contains(t) }
        .sortedBy { if (it.title.lowercase().startsWith(t)) 0 else 1 }
        .distinctBy { it.title.lowercase() }.take(max)
}

/** A list's items, the add field, and (wide screens) the details pane. */
@Composable
fun ListBody(env: Env, list: ListItem, lists: List<ListItem>, shopping: Boolean, snackbar: SnackbarHostState, addFocus: FocusRequester, wide: Boolean, modifier: Modifier, sort: Sort = Sort.Order, openFirst: String? = null) {
    val node = env.node
    val scope = rememberCoroutineScope()
    val tasks = remember(node, env.version, list.id) { runCatching { node.tasks(list.id) }.getOrDefault(emptyList()) }
    val settling = remember(list.id) { mutableStateListOf<String>() }
    val hidden = remember(list.id) { mutableStateListOf<String>() } // deleted, Undo still possible
    var openTask by remember(list.id, openFirst) { mutableStateOf(openFirst) }
    var showDone by remember { mutableStateOf(false) }
    val sec = sections(tasks, settling.toSet(), hidden.toSet(), if (shopping) Sort.Order else sort)
    var display by remember(list.id) { mutableStateOf(sec.active) }
    LaunchedEffect(tasks, settling.toList(), hidden.toList(), sort) { display = sec.active }
    // like Google Tasks: tasks are dragged into place only in "My order"
    val canDrag = shopping || sort == Sort.Order

    fun toggle(t: TaskItem) {
        val done = !t.done
        val next = tickTask(node, list.id, t, done)
        if (next != null) {
            // a repeating task stays in the list, due again on its next date
            env.changed()
            scope.launch { snackbar.showSnackbar(tr("“{title}” again {when}", "title" to t.title, "when" to whenLabel(next)), duration = SnackbarDuration.Short) }
            return
        }
        if (done && t.parentId == null) settling.add(t.id)
        env.changed()
        if (done && t.parentId == null) scope.launch {
            delay(SETTLE_MS); settling.remove(t.id)
        }
    }

    fun delete(t: TaskItem) {
        hidden.add(t.id)
        if (openTask == t.id) openTask = null
        scope.launch {
            val r = snackbar.showSnackbar(tr("Deleted “{title}”", "title" to t.title), actionLabel = tr("Undo"), duration = SnackbarDuration.Short)
            if (r != SnackbarResult.ActionPerformed) runCatching { node.deleteTask(list.id, t.id) }
            hidden.remove(t.id)
            env.changed()
        }
    }

    val lazy = rememberLazyListState()
    val reorder = rememberReorderableLazyListState(lazy) { from, to ->
        val a = display.getOrNull(from.index) ?: return@rememberReorderableLazyListState
        val b = display.getOrNull(to.index) ?: return@rememberReorderableLazyListState
        if (a.parentId == b.parentId) display = display.toMutableList().apply { add(to.index, removeAt(from.index)) }
    }
    fun drop(id: String) {
        val i = display.indexOfFirst { it.id == id }
        if (i < 0) return
        val parent = display[i].parentId
        val next = display.drop(i + 1).firstOrNull { it.parentId == parent }
        val prev = display.take(i).lastOrNull { it.parentId == parent }
        when {
            next != null -> node.moveTaskNextTo(list.id, id, next.id, false)
            prev != null -> node.moveTaskNextTo(list.id, id, prev.id, true)
            else -> return
        }
        env.changed()
    }

    Row(modifier) {
        Column(Modifier.weight(1f).fillMaxHeight()) {
            LazyColumn(Modifier.weight(1f).testTag("tasks"), state = lazy) {
                if (display.isEmpty() && sec.completed.isEmpty()) {
                    item(key = "empty") {
                        Text(
                            if (lists.size == 1 && tasks.isEmpty()) tr("Add your first task below") else tr("Nothing here yet"),
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.fillMaxWidth().padding(24.dp).testTag("empty"),
                        )
                    }
                }
                items(display, key = { it.id }) { t ->
                    ReorderableItem(reorder, key = t.id) { dragging ->
                        val handle = if (canDrag) Modifier.longPressDraggableHandle(onDragStopped = { drop(t.id) }) else Modifier
                        SwipeRow(onDelete = { delete(t) }) {
                            TaskRow(env, list.id, t, dragging, selected = wide && openTask == t.id, handle, star = !shopping, onToggle = { toggle(t) }, onOpen = { openTask = t.id })
                        }
                    }
                }
                if (sec.completed.isNotEmpty()) {
                    item(key = "completedHeader") {
                        Row(
                            Modifier.fillMaxWidth().clickable { showDone = !showDone }.padding(horizontal = 16.dp, vertical = 12.dp).testTag("completed"),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Icon(if (showDone) Ic.Down else Ic.Right, null, Modifier.size(20.dp))
                            Spacer(Modifier.width(8.dp))
                            Text(tr("Completed ({n})", "n" to sec.completed.count { it.parentId == null }), style = MaterialTheme.typography.titleSmall)
                        }
                    }
                    if (showDone) items(sec.completed, key = { "c" + it.id }) { t ->
                        SwipeRow(onDelete = { delete(t) }) {
                            TaskRow(env, list.id, t, false, selected = wide && openTask == t.id, Modifier, star = false, onToggle = { toggle(t) }, onOpen = { openTask = t.id })
                        }
                    }
                }
            }
            HorizontalDivider()
            AddBar(env, list, tasks, shopping, addFocus)
        }
        val selected = tasks.find { it.id == openTask && it.id !in hidden }
        if (wide && selected != null) {
            VerticalDivider()
            Column(Modifier.width(360.dp).fillMaxHeight().verticalScroll(rememberScrollState()).padding(16.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Spacer(Modifier.weight(1f))
                    IconButton(onClick = { openTask = null }, modifier = Modifier.testTag("detailsClose")) { Icon(Ic.Close, tr("Close")) }
                }
                Details(env, list, lists, tasks, selected, onDelete = { delete(selected) }, onMoved = { openTask = null }, onOpen = { openTask = it })
            }
        }
    }
    if (!wide && selected(tasks, openTask, hidden) != null) {
        DetailsSheet(env, list, lists, tasks, selected(tasks, openTask, hidden)!!, onDelete = { t -> delete(t) }, onClose = { openTask = null }, onOpen = { openTask = it })
    }
}

private fun selected(tasks: List<TaskItem>, id: String?, hidden: List<String>) = tasks.find { it.id == id && it.id !in hidden }

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun DetailsSheet(env: Env, list: ListItem, lists: List<ListItem>, tasks: List<TaskItem>, t: TaskItem, onDelete: (TaskItem) -> Unit, onClose: () -> Unit, onOpen: (String) -> Unit) {
    ModalBottomSheet(
        onDismissRequest = onClose, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        dragHandle = {
            Box(Modifier.fillMaxWidth()) {
                androidx.compose.material3.BottomSheetDefaults.DragHandle(Modifier.align(Alignment.Center))
                IconButton(onClick = onClose, modifier = Modifier.align(Alignment.CenterEnd).padding(end = 4.dp).testTag("detailsClose")) { Icon(Ic.Close, tr("Close")) }
            }
        },
    ) {
        Column(Modifier.padding(horizontal = 16.dp).padding(bottom = 24.dp).verticalScroll(rememberScrollState())) {
            Details(env, list, lists, tasks, t, onDelete = { onDelete(t) }, onMoved = onClose, onOpen = onOpen)
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SwipeRow(onDelete: () -> Unit, content: @Composable () -> Unit) {
    val state = rememberSwipeToDismissBoxState(confirmValueChange = { v ->
        if (v != SwipeToDismissBoxValue.Settled) onDelete()
        false // the row is hidden by the caller (with Undo), so it never stays swiped
    })
    SwipeToDismissBox(state, backgroundContent = {
        Row(Modifier.fillMaxSize().background(MaterialTheme.colorScheme.errorContainer).padding(horizontal = 20.dp), verticalAlignment = Alignment.CenterVertically) {
            Icon(Ic.Delete, tr("Delete"), tint = MaterialTheme.colorScheme.onErrorContainer)
            Spacer(Modifier.weight(1f))
            Icon(Ic.Delete, tr("Delete"), tint = MaterialTheme.colorScheme.onErrorContainer)
        }
    }) { content() }
}

/** A round tick box, larger than the text so it's easy to hit. */
@Composable
fun Tick(done: Boolean, tag: String, onClick: () -> Unit) {
    val c = MaterialTheme.colorScheme
    Box(Modifier.size(44.dp).clip(CircleShape).clickable(onClick = onClick).testTag(tag), contentAlignment = Alignment.Center) {
        Box(
            Modifier.size(22.dp).clip(CircleShape)
                .background(if (done) c.primary else Color.Transparent)
                .border(2.dp, if (done) c.primary else c.outline, CircleShape),
            contentAlignment = Alignment.Center,
        ) { if (done) Icon(Ic.Check, tr("Done"), tint = c.onPrimary, modifier = Modifier.size(16.dp)) }
    }
}

@Composable
private fun TaskRow(env: Env, listId: String, t: TaskItem, dragging: Boolean, selected: Boolean, handle: Modifier, star: Boolean, onToggle: () -> Unit, onOpen: () -> Unit) {
    val c = MaterialTheme.colorScheme
    val bg = when {
        dragging -> c.surfaceContainerHigh
        selected -> c.secondaryContainer
        else -> c.surface
    }
    Row(
        Modifier.fillMaxWidth().background(bg).then(handle).clickable(onClick = onOpen)
            .padding(start = (4 + t.depth.toInt() * 28).dp, end = 12.dp).heightIn(min = 52.dp).testTag("task-" + t.title),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Tick(t.done, "done-" + t.title, onToggle)
        val marks = c.onSurfaceVariant
        Column(Modifier.weight(1f).padding(vertical = 8.dp)) {
            Text(
                t.title,
                color = if (t.done) c.onSurfaceVariant else c.onSurface,
                textDecoration = if (t.done) TextDecoration.LineThrough else null,
                maxLines = 3, overflow = TextOverflow.Ellipsis,
            )
            val note = t.notes.lineSequence().firstOrNull { it.isNotBlank() }?.trim()
            val inDay = !t.done && inMyDay(t)
            if (inDay || t.remindMs != null || note != null || t.hasAttachment) Row(verticalAlignment = Alignment.CenterVertically) {
                if (inDay) {
                    Icon(Ic.Sun, null, tint = marks, modifier = Modifier.padding(end = 4.dp).size(14.dp))
                    Text(tr("My Day") + if (t.remindMs != null || note != null || t.hasAttachment) "  ·  " else "", style = MaterialTheme.typography.labelMedium, color = marks, modifier = Modifier.testTag("inDay-" + t.title))
                }
                t.remindMs?.let { ms ->
                    val at = fromMillis(ms.toLong())
                    val late = !t.done && at.isBefore(LocalDateTime.now())
                    Icon(Ic.Bell, null, tint = if (late) c.error else marks, modifier = Modifier.padding(end = 4.dp).size(14.dp))
                    Text((if (t.repeat != null) "↻ " else "") + whenLabel(at), style = MaterialTheme.typography.labelMedium, color = if (late) c.error else marks, modifier = Modifier.testTag("when-" + t.title))
                }
                val text = note ?: if (t.hasAttachment) tr("Notes") else null
                if (text != null) Text(
                    (if (t.remindMs != null) "  ·  " else "") + text, style = MaterialTheme.typography.bodySmall, color = marks,
                    maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f, fill = false).testTag("notePeek-" + t.title),
                )
            }
        }
        if (t.images.isNotEmpty()) Icon(Ic.Image, tr("Has pictures"), tint = marks, modifier = Modifier.padding(start = 6.dp).size(16.dp))
        t.assigned?.let { p -> SmallAvatar(p, modifier = Modifier.padding(start = 8.dp).testTag("assignee-" + t.title)) }
        if (star) StarButton(t.starredMs != null, "star-" + t.title) { setStar(env.node, listId, t, t.starredMs == null); env.changed() }
    }
}

/** The always-there add field: Enter adds and stays open; "when" words become a reminder. */
@Composable
private fun AddBar(env: Env, list: ListItem, tasks: List<TaskItem>, shopping: Boolean, focus: FocusRequester) {
    val node = env.node
    var text by remember(list.id) { mutableStateOf("") }
    var ignoreWhen by remember(list.id) { mutableStateOf(false) }
    val parsed = if (ignoreWhen || shopping) null else parseWhen(text)
    val suggested = if (shopping) suggestions(tasks, text) else emptyList()

    fun bringBack(t: TaskItem) {
        runCatching { node.setDone(list.id, t.id, false) }
        text = ""
        env.changed()
    }

    fun add() {
        val raw = text.trim()
        if (raw.isEmpty()) return
        if (shopping) {
            tasks.firstOrNull { it.done && it.parentId == null && it.title.equals(raw, ignoreCase = true) }?.let { bringBack(it); return }
        }
        val title = parsed?.title?.ifBlank { raw } ?: raw
        val id = runCatching { node.addTask(list.id, title, null) }.getOrNull()
        if (id != null && parsed != null) runCatching { node.setReminder(list.id, id, parsed.at.millis().toULong()) }
        if (id != null && parsed?.repeat != null) runCatching { node.setRepeat(list.id, id, parsed.repeat) }
        text = ""; ignoreWhen = false
        env.changed()
    }

    Column(Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 6.dp)) {
        if (suggested.isNotEmpty()) {
            Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                for (s in suggested) SuggestionChip({ bringBack(s) }, label = { Text("${s.title}  ↺") }, modifier = Modifier.testTag("again-" + s.title))
            }
        }
        AddField(text, { text = it; if (it.isEmpty()) ignoreWhen = false }, parsed, { ignoreWhen = true }, if (shopping) tr("Add an item") else tr("Add a task"), focus, ::add)
    }
}

/** The add field itself: a pill with +, the reminder its words make shown as a chip above it (tap to drop it). */
@Composable
internal fun AddField(text: String, onText: (String) -> Unit, parsed: When?, onIgnore: () -> Unit, placeholder: String, focus: FocusRequester, add: () -> Unit) {
    Column {
        if (parsed != null) {
            InputChip(
                true, onIgnore,
                label = { Text(tr("Remind {when}", "when" to whenLabel(parsed.at)) + (parsed.repeat?.let { ", " + Repeat.label(it).lowercase() } ?: "")) },
                leadingIcon = { Icon(Ic.Bell, null, Modifier.size(16.dp)) },
                trailingIcon = { Icon(Ic.Close, tr("Don't remind"), Modifier.size(16.dp)) },
                modifier = Modifier.testTag("whenChip"),
            )
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
            TextField(
                text, onText,
                placeholder = { Text(placeholder) },
                singleLine = true,
                leadingIcon = { Icon(Ic.Add, null) },
                keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Sentences, imeAction = ImeAction.Done),
                keyboardActions = KeyboardActions(onDone = { add() }),
                colors = TextFieldDefaults.colors(
                    focusedContainerColor = MaterialTheme.colorScheme.surfaceContainerHigh, unfocusedContainerColor = MaterialTheme.colorScheme.surfaceContainerHigh,
                    focusedIndicatorColor = Color.Transparent, unfocusedIndicatorColor = Color.Transparent,
                ),
                shape = RoundedCornerShape(28.dp),
                modifier = Modifier.weight(1f).focusRequester(focus).testTag("newtask")
                    .onEnterKey { add() },
            )
            if (text.isNotBlank()) IconButton(onClick = add, modifier = Modifier.testTag("addtask")) { Icon(Ic.Check, tr("Add"), tint = MaterialTheme.colorScheme.primary) }
        }
    }
}

/** Enter on a hardware keyboard adds (desktop), as the keyboard's Done does on phones. */
private fun Modifier.onEnterKey(action: () -> Unit) = onPreviewKeyEvent { e ->
    if (e.type == KeyEventType.KeyDown && (e.key == Key.Enter || e.key == Key.NumPadEnter)) {
        action(); true
    } else false
}
