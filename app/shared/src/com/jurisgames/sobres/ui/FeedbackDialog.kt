package com.jurisgames.sobres.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Checkbox
import androidx.compose.material3.FilterChip
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
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import com.jurisgames.sobres.core.KtNode
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.net.HttpURLConnection
import java.net.URI

/**
 * "Send feedback", the same as in every free Jurisgames app (github.com/kleiders3010/jurisgames-feedback): a problem,
 * an idea or something else, an optional email for an answer and, if the tick stays on, the diagnostics (engineering
 * state only, never task content). "See what's sent" shows exactly what goes. The report is emailed to
 * hello@jurisgames.com and not stored.
 */
object Feedback {
    /** Where reports go; tests point it at a local server. */
    const val LIVE = "https://feedback.jurisgames.com/send"
    /** Tests point it at a server on this computer; any other address is ignored (APP-RULES safety 1). */
    var endpoint = LIVE
    private fun target(): String {
        val host = runCatching { URI(endpoint).host }.getOrNull()
        return if (endpoint == LIVE || host == "127.0.0.1" || host == "localhost") endpoint else LIVE
    }
    val kinds = listOf("bug" to "Something isn't working", "idea" to "An idea", "other" to "Something else")

    private fun str(s: String) = buildString {
        append('"')
        for (c in s) when {
            c == '"' -> append("\\\"")
            c == '\\' -> append("\\\\")
            c == '\n' -> append("\\n")
            c == '\r' -> append("\\r")
            c == '\t' -> append("\\t")
            c < ' ' -> append("\\u%04x".format(c.code))
            else -> append(c)
        }
        append('"')
    }

    /** The report as JSON (the endpoint's format), pretty enough to read in "See what's sent". */
    fun json(kind: String, message: String, email: String, version: String, details: String?): String = buildString {
        append("{\n  \"app\": \"sobres\",\n  \"kind\": ${str(kind)},\n  \"message\": ${str(message.trim())},\n")
        append("  \"lang\": ${str(java.util.Locale.getDefault().toLanguageTag())},\n  \"version\": ${str(version)}")
        if (email.isNotBlank()) append(",\n  \"email\": ${str(email.trim())}")
        if (details != null) {
            val vm = System.getProperty("java.vm.name") ?: ""
            val platform = if (vm.contains("Dalvik", true) || vm.contains("ART", false)) "android" else "desktop"
            append(",\n  \"details\": {\n    \"platform\": ${str(platform)},\n")
            append("    \"system\": ${str("${System.getProperty("os.name")} ${System.getProperty("os.version")}")},\n")
            append("    \"diagnostics\": [\n")
            append(details.trimEnd().lines().joinToString(",\n") { "      " + str(it) })
            append("\n    ]\n  }")
        }
        append("\n}")
    }

    /** Sends it; returns null when it went, or a sentence to show. */
    fun send(body: String): String? = try {
        val c = URI(target()).toURL().openConnection() as HttpURLConnection
        c.requestMethod = "POST"
        c.doOutput = true
        c.connectTimeout = 15_000
        c.readTimeout = 20_000
        c.setRequestProperty("Content-Type", "application/json")
        c.outputStream.use { it.write(body.toByteArray()) }
        val code = c.responseCode
        val answer = (if (code < 400) c.inputStream else c.errorStream)?.bufferedReader()?.readText() ?: ""
        when {
            code == 200 && answer.contains("\"ok\":true") -> null
            code == 429 -> tr("Too many reports from this network today. Please try again tomorrow, or write to hello@jurisgames.com.")
            answer.contains("\"email\"") -> tr("That email doesn't look right.")
            answer.contains("\"details\"") -> tr("The technical details are too big. Untick them and send again.")
            else -> tr("It couldn't be sent. Check your connection and try again, or write to hello@jurisgames.com.")
        }
    } catch (e: Exception) {
        tr("It couldn't be sent. Check your connection and try again, or write to hello@jurisgames.com.")
    }
}

@Composable
fun FeedbackDialog(node: KtNode, onSent: () -> Unit, onClose: () -> Unit) {
    var kind by remember { mutableStateOf("bug") }
    var message by remember { mutableStateOf("") }
    var email by remember { mutableStateOf("") }
    var include by remember { mutableStateOf(true) }
    var preview by remember { mutableStateOf(false) }
    var sending by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf("") }
    val diag = remember(node) { node.diagnostics() }
    val details = remember(diag) { diagnosticsText(diag) }
    val body = Feedback.json(kind, message, email, diag.version, if (include) details else null)
    val scope = rememberCoroutineScope()
    AlertDialog(
        onDismissRequest = onClose,
        confirmButton = {
            TextButton(enabled = !sending, modifier = Modifier.testTag("feedbackSend"), onClick = {
                if (message.isBlank()) {
                    error = tr("Write a few words first.")
                    return@TextButton
                }
                sending = true
                error = ""
                scope.launch {
                    val problem = withContext(Dispatchers.IO) { Feedback.send(body) }
                    sending = false
                    if (problem == null) onSent() else error = problem
                }
            }) { Text(if (sending) tr("Sending…") else tr("Send")) }
        },
        dismissButton = { TextButton(onClick = onClose) { Text(tr("Cancel")) } },
        title = { Text(tr("Send feedback")) },
        text = {
            Column(Modifier.heightIn(max = 520.dp).verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(tr("It goes to the person who makes this app. Thanks for helping!"), style = MaterialTheme.typography.bodySmall)
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    for ((k, label) in Feedback.kinds) {
                        FilterChip(selected = kind == k, onClick = { kind = k }, label = { Text(tr(label)) }, modifier = Modifier.testTag("feedbackKind-$k"))
                    }
                }
                OutlinedTextField(message, { message = it.take(4000) }, placeholder = { Text(tr("What happened, or what would make it better?")) },
                    minLines = 4, modifier = Modifier.fillMaxWidth().testTag("feedbackMessage"))
                OutlinedTextField(email, { email = it.take(254) }, placeholder = { Text(tr("Your email, if you'd like an answer (optional)")) },
                    singleLine = true, modifier = Modifier.fillMaxWidth().testTag("feedbackEmail"))
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Checkbox(include, { include = it }, modifier = Modifier.testTag("feedbackDetails"))
                    Text(tr("Include technical details"))
                }
                Text(tr("Sync state, relays, storage sizes, app version and system. Never your budget, entries or names."), style = MaterialTheme.typography.bodySmall)
                TextButton(onClick = { preview = !preview }, modifier = Modifier.testTag("feedbackSee")) {
                    Text(if (preview) tr("Hide what's sent") else tr("See what's sent"))
                }
                if (preview) Text(body, style = MaterialTheme.typography.bodySmall, modifier = Modifier.testTag("feedbackPreview"))
                if (error.isNotEmpty()) Text(error, color = MaterialTheme.colorScheme.error, modifier = Modifier.testTag("feedbackError"))
            }
        },
    )
}
