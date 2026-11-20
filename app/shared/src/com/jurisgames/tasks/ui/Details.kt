package com.jurisgames.tasks.ui

import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.AssistChip
import androidx.compose.material3.DatePicker
import androidx.compose.material3.DatePickerDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.InputChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TimePicker
import androidx.compose.material3.rememberDatePickerState
import androidx.compose.material3.rememberTimePickerState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.unit.dp
import com.jurisgames.tasks.core.KtNode
import com.jurisgames.tasks.core.ListItem
import com.jurisgames.tasks.core.TaskItem
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext
import java.time.DayOfWeek
import java.time.Instant
import java.time.LocalDateTime
import java.time.LocalTime
import java.time.ZoneOffset
import java.time.temporal.TemporalAdjusters

/** Quick reminder choices, as (label, moment). "Later today" only while the day has room. */
fun reminderChoices(now: LocalDateTime = LocalDateTime.now()): List<Pair<String, LocalDateTime>> {
    val out = mutableListOf<Pair<String, LocalDateTime>>()
    val later = now.plusHours(3).withMinute(0).withSecond(0).withNano(0)
    if (later.toLocalDate() == now.toLocalDate() && later.hour <= 21) out += "Later today" to later
    out += "Tomorrow" to now.toLocalDate().plusDays(1).atTime(MORNING)
    out += "Next week" to now.toLocalDate().with(TemporalAdjusters.next(DayOfWeek.MONDAY)).atTime(MORNING)
    return out
}

/**
 * Everything about one item, all visible at once (UI-SPEC "Details sheet"): title, reminder,
 * notes, pictures, subtasks, move, delete. Changes save as they're made.
 */
@OptIn(ExperimentalMaterial3Api::class, androidx.compose.foundation.layout.ExperimentalLayoutApi::class)
@Composable
fun Details(env: Env, list: ListItem, lists: List<ListItem>, tasks: List<TaskItem>, t: TaskItem, onDelete: () -> Unit, onMoved: () -> Unit, onOpen: (String) -> Unit) {
    val node = env.node
    Column(verticalArrangement = Arrangement.spacedBy(14.dp)) {
        // Title: saved after a short pause in typing.
        var title by remember(t.id) { mutableStateOf(t.title) }
        LaunchedEffect(t.id, title) {
            delay(500)
            if (title.isNotBlank() && title.trim() != t.title) {
                runCatching { node.renameTask(list.id, t.id, title.trim()) }; env.changed()
            }
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
            Tick(t.done, "detailDone") { tickTask(node, list.id, t, !t.done); env.changed() }
            OutlinedTextField(
                title, { title = it }, textStyle = MaterialTheme.typography.titleMedium,
                keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Sentences),
                modifier = Modifier.weight(1f).testTag("edit-" + t.title),
            )
            if (list.listType != "shopping") StarButton(t.starredMs != null, "detailStar") { setStar(node, list.id, t, t.starredMs == null); env.changed() }
        }
        if (list.listType != "shopping" && !t.done) {
            val inDay = inMyDay(t)
            androidx.compose.material3.FilterChip(
                inDay, { setMyDay(node, list.id, t, !inDay); env.changed() },
                label = { Text(if (inDay) tr("Added to My Day") else tr("Add to My Day")) },
                leadingIcon = { Icon(Ic.Sun, null, Modifier.size(18.dp), tint = if (inDay) SunColor else MaterialTheme.colorScheme.onSurfaceVariant) },
                trailingIcon = if (inDay) ({ Icon(Ic.Close, tr("Remove from My Day"), Modifier.size(16.dp)) }) else null,
                modifier = Modifier.testTag("detailMyDay"),
            )
        }

        // Assign to (shared lists only, like Microsoft To Do)
        val shared = remember(node, env.version, list.id) { isShared(node, list) }
        if (shared && list.listType != "shopping") Section(Ic.Person, tr("Assign to")) { AssignTo(env, list, t) }

        // Reminder
        Section(Ic.Bell, tr("Remind me")) {
            var picking by remember(t.id) { mutableStateOf(false) }
            fun set(at: LocalDateTime?) {
                runCatching { node.setReminder(list.id, t.id, at?.millis()?.toULong()) }; env.changed()
            }
            FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                t.remindMs?.let { ms ->
                    InputChip(true, { set(null) }, label = { Text(whenLabel(fromMillis(ms.toLong()))) }, trailingIcon = { Icon(Ic.Close, tr("No reminder"), Modifier.size(16.dp)) }, modifier = Modifier.testTag("remindClear"))
                }
                for ((label, at) in reminderChoices()) AssistChip({ set(at) }, label = { Text(tr(label)) }, modifier = Modifier.testTag("remind-$label"))
                AssistChip({ picking = true }, label = { Text(tr("Pick…")) }, modifier = Modifier.testTag("remindPick"))
            }
            if (picking) PickMoment(t.remindMs?.let { fromMillis(it.toLong()) }, onPicked = { set(it); picking = false }, onClose = { picking = false })
        }

        // Repeat (like Todoist's "every"): ticked off, it comes back on its next date
        Section(Ic.Repeat, tr("Repeat")) {
            fun pick(rule: String?) {
                runCatching {
                    if (rule != null && t.remindMs == null) node.setReminder(list.id, t.id, firstDate(rule).millis().toULong())
                    node.setRepeat(list.id, t.id, rule)
                }
                env.changed()
            }
            FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                for (rule in Repeat.CHOICES) {
                    val on = t.repeat == rule
                    FilterChip(on, { pick(if (on) null else rule) }, label = { Text(Repeat.label(rule)) }, modifier = Modifier.testTag("repeat-$rule"))
                }
            }
        }

        // Notes (long notes are an attachment: read-only until all of it is here)
        Section(Ic.Notes, tr("Notes")) {
            val notes = remember(list.id, t.id, env.version) { runCatching { node.notes(list.id, t.id) }.getOrNull() }
            val complete = notes?.complete ?: true
            var text by remember(t.id, complete) { mutableStateOf(notes?.text ?: "") }
            LaunchedEffect(t.id, text) {
                delay(600)
                if (complete && text != (notes?.text ?: "")) {
                    runCatching { node.setNotes(list.id, t.id, text) }; env.changed()
                }
            }
            OutlinedTextField(text, { text = it }, enabled = complete, placeholder = { Text(tr("Add notes")) }, minLines = 2, modifier = Modifier.fillMaxWidth().heightIn(max = 280.dp).testTag("notesText"))
            if (!complete) {
                val kb = ((notes?.size ?: 0u).toLong() + 1023) / 1024
                Text(tr("The rest of this note ({kb} KB) is still on its way.", "kb" to kb), style = MaterialTheme.typography.bodySmall, modifier = Modifier.testTag("notesPending"))
            }
        }

        // Pictures
        if (env.onPickImage != null || t.images.isNotEmpty()) Section(Ic.Image, tr("Pictures")) {
            var viewing by remember(t.id) { mutableStateOf<String?>(null) }
            viewing?.let { img -> ImageViewer(node, list.id, t.id, img, env.version, env.changed) { viewing = null } }
            Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
                for (img in t.images) Thumbnail(node, list.id, img, env.version, Modifier.clickable { viewing = img }.testTag("thumb-" + t.title))
                env.onPickImage?.let { pick ->
                    TextButton(onClick = { pick { bytes -> runCatching { node.addImage(list.id, t.id, bytes) }; env.changed() } }, modifier = Modifier.testTag("image-" + t.title)) { Text(tr("Add picture")) }
                }
            }
        }

        // Subtasks
        Section(Ic.Subtask, tr("Steps")) {
            val children = tasks.filter { it.parentId == t.id }
            for (c in children) {
                Row(Modifier.fillMaxWidth().clickable { onOpen(c.id) }, verticalAlignment = Alignment.CenterVertically) {
                    Tick(c.done, "done-" + c.title) { runCatching { node.setDone(list.id, c.id, !c.done) }; env.changed() }
                    Text(c.title, Modifier.weight(1f))
                }
            }
            var step by remember(t.id) { mutableStateOf("") }
            fun addStep() {
                if (step.isBlank()) return
                runCatching { node.addTask(list.id, step.trim(), t.id) }; step = ""; env.changed()
            }
            OutlinedTextField(
                step, { step = it }, placeholder = { Text(tr("Add a step")) }, singleLine = true,
                keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Sentences, imeAction = ImeAction.Done),
                keyboardActions = KeyboardActions(onDone = { addStep() }),
                modifier = Modifier.fillMaxWidth().testTag("newStep"),
            )
        }

        Row(verticalAlignment = Alignment.CenterVertically) {
            val others = lists.filter { it.id != list.id }
            if (others.isNotEmpty() && t.parentId == null) {
                Box {
                    var open by remember { mutableStateOf(false) }
                    TextButton(onClick = { open = true }, modifier = Modifier.testTag("moveTo")) { Text(tr("Move to list")) }
                    DropdownMenu(open, { open = false }) {
                        for (o in others) DropdownMenuItem(text = { Text(o.name) }, onClick = {
                            open = false
                            moveTask(node, list.id, t, tasks, o.id)
                            env.changed(); onMoved()
                        }, modifier = Modifier.testTag("moveTo-" + o.name))
                    }
                }
            }
            Spacer(Modifier.weight(1f))
            TextButton(onClick = onDelete, modifier = Modifier.testTag("delete-" + t.title), colors = androidx.compose.material3.ButtonDefaults.textButtonColors(contentColor = MaterialTheme.colorScheme.error)) {
                Icon(Ic.Delete, null, Modifier.size(18.dp)); Spacer(Modifier.size(6.dp)); Text(tr("Delete"))
            }
        }
    }
}

@Composable
private fun Section(icon: androidx.compose.ui.graphics.vector.ImageVector, label: String, content: @Composable () -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Icon(icon, null, Modifier.size(18.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
            Spacer(Modifier.size(8.dp))
            Text(label, style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        content()
    }
}

/** Copy an item (with its steps, notes, reminder, pictures) into another list, then remove it here. */
fun moveTask(node: KtNode, from: String, t: TaskItem, all: List<TaskItem>, to: String) {
    fun copy(x: TaskItem, parent: String?) {
        val id = node.addTask(to, x.title, parent)
        runCatching { node.notes(from, x.id) }.getOrNull()?.takeIf { it.complete && it.text.isNotEmpty() }?.let { node.setNotes(to, id, it.text) }
        x.remindMs?.let { node.setReminder(to, id, it) }
        x.repeat?.let { node.setRepeat(to, id, it) }
        for (img in x.images) runCatching { node.taskImage(from, img) }.getOrNull()?.let { runCatching { node.addImage(to, id, it) } }
        if (x.done) node.setDone(to, id, true)
        for (c in all.filter { it.parentId == x.id }) copy(c, id)
    }
    runCatching {
        copy(t, null)
        node.deleteTask(from, t.id)
    }
}

/** Pick a day, then a time. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun PickMoment(initial: LocalDateTime?, onPicked: (LocalDateTime) -> Unit, onClose: () -> Unit) {
    val start = initial ?: LocalDateTime.now().plusDays(1).with(MORNING)
    var day by remember { mutableStateOf<java.time.LocalDate?>(null) }
    if (day == null) {
        val ds = rememberDatePickerState(initialSelectedDateMillis = start.toLocalDate().atStartOfDay().toInstant(ZoneOffset.UTC).toEpochMilli())
        DatePickerDialog(
            onDismissRequest = onClose,
            confirmButton = {
                TextButton(onClick = {
                    ds.selectedDateMillis?.let { day = Instant.ofEpochMilli(it).atZone(ZoneOffset.UTC).toLocalDate() }
                }, modifier = Modifier.testTag("pickDay")) { Text(tr("Next")) }
            },
            dismissButton = { TextButton(onClick = onClose) { Text(tr("Cancel")) } },
        ) { DatePicker(ds) }
    } else {
        val ts = rememberTimePickerState(initialHour = start.hour, initialMinute = start.minute)
        AlertDialog(
            onDismissRequest = onClose,
            confirmButton = { TextButton(onClick = { onPicked(day!!.atTime(LocalTime.of(ts.hour, ts.minute))) }, modifier = Modifier.testTag("pickTime")) { Text(tr("Set")) } },
            dismissButton = { TextButton(onClick = onClose) { Text(tr("Cancel")) } },
            text = { TimePicker(ts) },
        )
    }
}

@Composable
private fun image(node: KtNode, listId: String, imageId: String, version: Int): ImageBitmap? {
    val decode = LocalImageDecoder.current
    val bitmap by produceState<ImageBitmap?>(null, imageId, version) {
        value = withContext(Dispatchers.IO) { runCatching { node.taskImage(listId, imageId) }.getOrNull()?.let { decode?.invoke(it) } }
    }
    return bitmap
}

@Composable
private fun Thumbnail(node: KtNode, listId: String, imageId: String, version: Int, modifier: Modifier) {
    val bitmap = image(node, listId, imageId, version)
    val shape = RoundedCornerShape(8.dp)
    if (bitmap != null) {
        Image(bitmap, contentDescription = tr("Picture"), contentScale = ContentScale.Crop, modifier = modifier.size(64.dp).clip(shape))
    } else {
        // Still on its way to this device (or not decodable here).
        Box(modifier.size(64.dp).clip(shape).background(MaterialTheme.colorScheme.surfaceVariant), contentAlignment = Alignment.Center) { Text("…") }
    }
}

@Composable
private fun ImageViewer(node: KtNode, listId: String, taskId: String, imageId: String, version: Int, onChanged: () -> Unit, onClose: () -> Unit) {
    val bitmap = image(node, listId, imageId, version)
    AlertDialog(
        onDismissRequest = onClose,
        confirmButton = { TextButton(onClick = onClose) { Text(tr("Close")) } },
        dismissButton = {
            TextButton(onClick = { node.removeImage(listId, taskId, imageId); onChanged(); onClose() }, modifier = Modifier.testTag("imageRemove")) { Text(tr("Remove")) }
        },
        text = {
            if (bitmap != null) Image(bitmap, contentDescription = tr("Picture"), contentScale = ContentScale.Fit, modifier = Modifier.fillMaxWidth().heightIn(max = 480.dp).testTag("imageFull"))
            else Text(tr("This picture is still on its way to this device."))
        },
    )
}
