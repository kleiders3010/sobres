package com.jurisgames.tasks.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.heightIn
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import com.jurisgames.tasks.core.KtNode

/**
 * A task's notes. Long notes are a text attachment (D4): if it hasn't arrived yet, the
 * preview shows and editing waits until the whole text is here (so nothing gets cut).
 */
@Composable
fun NotesDialog(node: KtNode, listId: String, taskId: String, version: Int, onSaved: () -> Unit, onClose: () -> Unit) {
    // Re-read when a sync brings the attachment in.
    val notes = remember(listId, taskId, version) { runCatching { node.notes(listId, taskId) }.getOrNull() }
    var text by remember(notes?.complete) { mutableStateOf(notes?.text ?: "") }
    var error by remember { mutableStateOf("") }
    val complete = notes?.complete ?: true
    AlertDialog(
        onDismissRequest = onClose,
        confirmButton = {
            TextButton(onClick = {
                try {
                    node.setNotes(listId, taskId, text)
                    onSaved()
                    onClose()
                } catch (e: Exception) {
                    error = e.message ?: tr("Couldn't save")
                }
            }, enabled = complete, modifier = Modifier.testTag("notesSave")) { Text(tr("Save")) }
        },
        dismissButton = { TextButton(onClick = onClose) { Text(tr("Close")) } },
        title = { Text(tr("Notes")) },
        text = {
            Column {
                OutlinedTextField(text, { text = it }, enabled = complete, modifier = Modifier.heightIn(min = 120.dp, max = 360.dp).testTag("notesText"))
                if (!complete) {
                    val kb = ((notes?.size ?: 0u).toLong() + 1023) / 1024
                    Text(tr("The rest of this note ({kb} KB) is still on its way.", "kb" to kb), style = MaterialTheme.typography.bodySmall, modifier = Modifier.testTag("notesPending"))
                }
                if (error.isNotEmpty()) Text(error, modifier = Modifier.testTag("notesError"))
            }
        },
    )
}
