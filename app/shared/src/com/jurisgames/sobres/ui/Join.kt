package com.jurisgames.sobres.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

const val CODE_WORDS = 8

/** Playful lines while an invite is being found (UX-NOTES "Waiting and loading"). */
val FINDING = listOf(tr("Finding the list…"), tr("Asking around…"), tr("Knocking on doors…"), tr("Following the trail…"), tr("Almost there, probably…"))

/**
 * What a typed word box should become: the full word once it is certain (4 letters typed, or
 * a whole short word), null while still typing, or "" when the letters match no word (a typo).
 */
fun settleWord(typed: String, complete: (String) -> String?): String? {
    val t = typed.trim().lowercase()
    if (t.length < 3) return null
    val w = complete(t.take(3)) ?: return ""
    if (!w.startsWith(t)) return ""
    return if (t == w || t.length >= 4) w else null
}

/**
 * "Join a shared list": one box per word; a word fills itself in and moves on once certain;
 * pasting a whole code or link anywhere fills every box and joins right away.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun JoinDialog(env: Env, prefilled: String?, onJoined: (String) -> Unit, onClose: () -> Unit) {
    val node = env.node
    val scope = rememberCoroutineScope()
    val words = remember { mutableStateListOf(*Array(CODE_WORDS) { "" }) }
    val focus = remember { List(CODE_WORDS) { FocusRequester() } }
    var link by remember { mutableStateOf<String?>(null) }
    var busy by remember { mutableStateOf(false) }
    var result by remember { mutableStateOf("") }
    var line by remember { mutableIntStateOf(0) }
    var slow by remember { mutableStateOf(false) }
    val clip = LocalClipboardManager.current

    fun join(code: String) {
        if (busy) return
        busy = true; result = ""; slow = false
        scope.launch {
            result = try {
                val r = withContext(Dispatchers.IO) { node.joinCode(code) }
                env.changed()
                when {
                    r.stillLooking -> tr("Not found yet. You can close this: the app keeps looking and tells you when you're in.")
                    r.joined -> { onJoined(r.groupName); tr("You're in: {group}", "group" to r.groupName) }
                    else -> tr("Asked to join {group} (from {who}). You're let in as soon as they're online.", "group" to r.groupName, "who" to r.inviterName)
                }
            } catch (e: Exception) {
                tr(e.message ?: tr("That code didn't work. Check it with the person who sent it."))
            }
            busy = false
        }
    }

    // A pasted code or link: fill everything and go.
    fun takeWhole(text: String): Boolean {
        val t = text.trim()
        if (t.startsWith("http") || t.contains("://")) {
            link = t; join(t); return true
        }
        val parts = t.split(Regex("[\\s\\-,.]+")).filter { it.isNotEmpty() }
        if (parts.size < CODE_WORDS) return false
        parts.take(CODE_WORDS).forEachIndexed { i, p -> words[i] = settleWord(p) { node.completeWord(it) }?.takeIf { it.isNotEmpty() } ?: p.lowercase() }
        join(words.joinToString(" "))
        return true
    }

    LaunchedEffect(prefilled) { prefilled?.let { takeWhole(it) } }
    LaunchedEffect(busy) {
        if (busy) {
            var n = 0
            while (busy) {
                delay(1200); n++; line = n % FINDING.size
                if (n >= 2) slow = true
            }
        }
    }

    AlertDialog(
        onDismissRequest = onClose,
        confirmButton = {
            TextButton(onClick = { join(link ?: words.joinToString(" ")) }, enabled = !busy && (link != null || words.all { it.isNotBlank() }), modifier = Modifier.testTag("joinGo")) { Text(tr("Join")) }
        },
        dismissButton = { TextButton(onClick = onClose) { Text(tr("Close")) } },
        title = { Text(tr("Join a shared list")) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(tr("Enter the code"), style = MaterialTheme.typography.bodyMedium)
                FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp), maxItemsInEachRow = 4) {
                    for (i in 0 until CODE_WORDS) {
                        val typo = settleWord(words[i]) { node.completeWord(it) } == ""
                        OutlinedTextField(
                            words[i],
                            { v ->
                                if (v.length > words[i].length + 3 && takeWhole(v)) return@OutlinedTextField
                                if (v.endsWith(" ") || v.endsWith("-")) {
                                    val s = settleWord(v) { node.completeWord(it) }
                                    if (!s.isNullOrEmpty()) { words[i] = s; if (i + 1 < CODE_WORDS) focus[i + 1].requestFocus() }
                                    return@OutlinedTextField
                                }
                                words[i] = v.lowercase()
                                val s = settleWord(v) { node.completeWord(it) }
                                if (!s.isNullOrEmpty()) {
                                    words[i] = s
                                    if (i + 1 < CODE_WORDS) focus[i + 1].requestFocus()
                                }
                            },
                            singleLine = true, isError = typo,
                            keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.None, autoCorrectEnabled = false, imeAction = if (i + 1 < CODE_WORDS) ImeAction.Next else ImeAction.Go),
                            modifier = Modifier.width(118.dp).focusRequester(focus[i]).testTag("word-$i"),
                        )
                    }
                }
                Row(verticalAlignment = Alignment.CenterVertically) {
                    TextButton(onClick = { clip.getText()?.text?.let { takeWhole(it) } }, modifier = Modifier.testTag("pasteCode")) { Text(tr("Paste")) }
                    env.onScan?.let { scan -> TextButton(onClick = { scan { takeWhole(it) } }, modifier = Modifier.testTag("scanJoin")) { Text(tr("or scan a QR code")) } }
                }
                if (busy) {
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                        CircularProgressIndicator(Modifier.width(20.dp), strokeWidth = 2.dp)
                        Column {
                            Text(FINDING[line], modifier = Modifier.testTag("joinFinding"))
                            if (slow) Text(tr("This can take a minute, keep the app open"), style = MaterialTheme.typography.bodySmall)
                        }
                    }
                }
                if (result.isNotEmpty()) Text(result, modifier = Modifier.testTag("joinResult"))
            }
        },
    )
}
