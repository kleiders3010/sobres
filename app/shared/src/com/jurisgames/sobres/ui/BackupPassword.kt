package com.jurisgames.sobres.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.input.PasswordVisualTransformation
import com.jurisgames.sobres.core.KtNode
import com.jurisgames.sobres.core.backupPasswordMinChars
import com.jurisgames.sobres.core.backupPasswordOpens
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * A backup waiting for its password: picked by the user, or restored by Android onto a new
 * phone (`KtNode.waitingBackup`). [SobresApp] asks for the password while it's set.
 */
object PendingRestore {
    var bytes by mutableStateOf<ByteArray?>(null)
}

/** A line the home screen shows once (desktop "Backup saved" / "Couldn't save the backup", safety audit S4-4). */
object Notice {
    var text by mutableStateOf<String?>(null)
}

/** Settings: set, change or remove the password backups need to open (audit T11). */
@Composable
fun BackupPasswordDialog(node: KtNode, onClose: () -> Unit) {
    val has = remember(node) { node.hasBackupPassword() }
    var pw by remember { mutableStateOf("") }
    var again by remember { mutableStateOf("") }
    var error by remember { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    val min = remember { backupPasswordMinChars().toInt() }
    fun save(value: String?) {
        busy = true
        scope.launch {
            val failed = withContext(Dispatchers.IO) { runCatching { node.setBackupPassword(value) }.exceptionOrNull() }
            busy = false
            if (failed == null) onClose() else error = tr(failed.message ?: "Couldn't save")
        }
    }
    AlertDialog(
        onDismissRequest = { if (!busy) onClose() },
        confirmButton = {
            TextButton(enabled = !busy, onClick = {
                error = when {
                    pw.length < min -> tr("At least {n} characters", "n" to min.toString()) // n>=2: pinned: backup::PW_MIN_CHARS = 8
                    pw != again -> tr("The passwords don't match")
                    else -> ""
                }
                if (error.isEmpty()) save(pw)
            }, modifier = Modifier.testTag("backupPwSave")) { Text(tr("Save")) }
        },
        dismissButton = {
            if (has) TextButton(enabled = !busy, onClick = { save(null) }, modifier = Modifier.testTag("backupPwRemove")) { Text(tr("Remove password")) }
            TextButton(enabled = !busy, onClick = onClose) { Text(tr("Cancel")) }
        },
        title = { Text(tr(if (has) "Change backup password" else "Backup password")) },
        text = {
            Column {
                Text(tr("Backups will need this password to open, also on a new phone. If you forget it, they can't be opened."), style = MaterialTheme.typography.bodySmall)
                OutlinedTextField(pw, { pw = it }, label = { Text(tr("Password")) }, singleLine = true, visualTransformation = PasswordVisualTransformation(), modifier = Modifier.testTag("backupPw"))
                OutlinedTextField(again, { again = it }, label = { Text(tr("Repeat it")) }, singleLine = true, visualTransformation = PasswordVisualTransformation(), modifier = Modifier.testTag("backupPwAgain"))
                if (error.isNotEmpty()) Text(error, modifier = Modifier.testTag("backupPwError"))
            }
        },
    )
}

/** Restoring a locked backup: its password, checked before the restore starts. */
@Composable
fun RestorePasswordDialog(backup: ByteArray, onRestore: (String) -> Unit, onCancel: () -> Unit) {
    var pw by remember { mutableStateOf("") }
    var wrong by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    AlertDialog(
        onDismissRequest = { if (!busy) onCancel() },
        confirmButton = {
            TextButton(enabled = !busy && pw.isNotEmpty(), onClick = {
                busy = true
                scope.launch {
                    val ok = withContext(Dispatchers.IO) { backupPasswordOpens(backup, pw) }
                    busy = false
                    if (ok) onRestore(pw) else wrong = true
                }
            }, modifier = Modifier.testTag("restorePwOk")) { Text(tr("Restore")) }
        },
        dismissButton = { TextButton(enabled = !busy, onClick = onCancel) { Text(tr("Not now")) } },
        title = { Text(tr("Your backup has a password")) },
        text = {
            Column {
                OutlinedTextField(pw, { pw = it; wrong = false }, label = { Text(tr("Password")) }, singleLine = true, visualTransformation = PasswordVisualTransformation(), modifier = Modifier.testTag("restorePw"))
                if (wrong) Text(tr("That password doesn't open this backup."), modifier = Modifier.testTag("restorePwWrong"))
            }
        },
    )
}
