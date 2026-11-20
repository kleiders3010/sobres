package com.jurisgames.tasks.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import com.jurisgames.tasks.core.KtNode
import com.jurisgames.tasks.core.ListItem
import com.jurisgames.tasks.core.PersonItem

private val periods = listOf("the last hour" to 3_600_000L, "the last day" to 86_400_000L, "the last 3 days" to 3 * 86_400_000L, "the last 30 days" to 30 * 86_400_000L)

/** "Undo changes by [person] since [time]" (DESIGN §4): other people's work stays. */
@Composable
fun UndoDialog(node: KtNode, list: ListItem, onDone: () -> Unit, onClose: () -> Unit) {
    val people = remember(list.id) { runCatching { node.people(list.id) }.getOrDefault(emptyList()) }
    var who by remember { mutableStateOf<PersonItem?>(null) }
    var since by remember { mutableStateOf(periods[1]) }
    var result by remember { mutableStateOf("") }
    AlertDialog(
        onDismissRequest = onClose,
        confirmButton = {
            TextButton(onClick = {
                val p = who ?: return@TextButton
                result = try {
                    val n = node.undoChangesBy(list.id, p, (System.currentTimeMillis() - since.second).toULong())
                    onDone()
                    if (n == 0u) tr("Nothing to undo") else (if (n == 1u) tr("Put back {n} change", "n" to n) else tr("Put back {n} changes", "n" to n))
                } catch (e: Exception) {
                    tr(e.message ?: tr("Couldn't undo"))
                }
            }, enabled = who != null, modifier = Modifier.testTag("undoGo")) { Text(tr("Undo")) }
        },
        dismissButton = { TextButton(onClick = onClose) { Text(tr("Close")) } },
        title = { Text(tr("Undo someone's changes")) },
        text = {
            Column {
                Text(tr("Whose changes?"), style = MaterialTheme.typography.titleSmall)
                for (p in people) {
                    FilterChip(
                        selected = who == p,
                        onClick = { who = p },
                        leadingIcon = { Avatar(p) },
                        label = { Text(personLabel(p, people)) },
                        modifier = Modifier.testTag("undoWho-" + p.name),
                    )
                }
                Text(tr("Made in…"), style = MaterialTheme.typography.titleSmall)
                Row {
                    for (period in periods) {
                        FilterChip(selected = since == period, onClick = { since = period }, label = { Text(tr(period.first.removePrefix("the last "))) }, modifier = Modifier.testTag("undoSince-" + period.second))
                    }
                }
                if (result.isNotEmpty()) Text(result, modifier = Modifier.testTag("undoResult"))
            }
        },
    )
}
