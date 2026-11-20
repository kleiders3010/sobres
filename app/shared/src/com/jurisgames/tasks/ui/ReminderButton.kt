package com.jurisgames.tasks.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
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
import java.text.DateFormat
import java.util.Calendar
import java.util.Date

/** A task's reminder: shows its time; set it for in an hour or tomorrow morning, or clear it. */
@Composable
fun ReminderButton(node: KtNode, listId: String, taskId: String, remindMs: ULong?, onChanged: () -> Unit) {
    var open by remember { mutableStateOf(false) }
    Column {
        TextButton(onClick = { open = true }, modifier = Modifier.testTag("remind-$taskId")) {
            Text(remindMs?.let { DateFormat.getDateTimeInstance(DateFormat.SHORT, DateFormat.SHORT).format(Date(it.toLong())) } ?: "⏰")
        }
        DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
            fun set(at: Long?) {
                node.setReminder(listId, taskId, at?.toULong())
                open = false
                onChanged()
            }
            DropdownMenuItem(text = { Text(tr("In 1 hour")) }, onClick = { set(System.currentTimeMillis() + 3_600_000) }, modifier = Modifier.testTag("remindHour"))
            DropdownMenuItem(text = { Text(tr("Tomorrow 9:00")) }, onClick = {
                val c = Calendar.getInstance().apply { add(Calendar.DAY_OF_YEAR, 1); set(Calendar.HOUR_OF_DAY, 9); set(Calendar.MINUTE, 0); set(Calendar.SECOND, 0) }
                set(c.timeInMillis)
            }, modifier = Modifier.testTag("remindTomorrow"))
            if (remindMs != null) DropdownMenuItem(text = { Text(tr("No reminder")) }, onClick = { set(null) }, modifier = Modifier.testTag("remindClear"))
        }
    }
}
