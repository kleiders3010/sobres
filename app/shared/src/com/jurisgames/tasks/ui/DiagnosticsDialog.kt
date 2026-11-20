package com.jurisgames.tasks.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.unit.dp
import com.jurisgames.tasks.core.Diagnostics
import com.jurisgames.tasks.core.KtNode
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter

private val time = DateTimeFormatter.ofPattern("yyyy-MM-dd HH:mm:ss").withZone(ZoneId.systemDefault())

/** The diagnostics as plain text: engineering state only, never secrets or task content. */
fun diagnosticsText(d: Diagnostics): String = buildString {
    appendLine("Tasks ${d.version} (feature level ${d.featureLevel})")
    appendLine()
    appendLine("Layers")
    d.layers.forEach { appendLine("  ${it.name}: ${it.state}") }
    appendLine()
    appendLine("Nostr relays")
    d.relays.forEach {
        appendLine("  ${it.url}: published ${it.publishedOk}, connect failures ${it.connectFailures} (${it.failuresInARow} in a row), read back ${it.readbackHits}/${it.readbackHits + it.readbackMisses}")
    }
    appendLine()
    appendLine("Helper relays in use: ${d.helperRelays.size}")
    appendLine("Storage helpers")
    d.helpers.forEach {
        val state = if (it.excluded) "excluded" else if (it.failing) "failing" else "ok"
        appendLine("  ${it.id}: $state, reached ${it.reachable}/${it.reachable + it.unreachable}, %.0f ms".format(it.latencyMs))
    }
    appendLine()
    appendLine("Storage")
    d.storage.forEach { appendLine("  ${it.name}: %.1f KB".format(it.bytes.toDouble() / 1024)) }
    appendLine()
    appendLine("Sync log (newest first)")
    d.rounds.forEach {
        appendLine("  ${time.format(Instant.ofEpochMilli(it.atMs.toLong()))}: sent ${it.written}, received ${it.received}, rejected ${it.rejected}, peers ${it.peersReached}, ${it.tookMs} ms")
    }
}

/** Hidden diagnostics screen (DESIGN §2): layers, relays, helpers, storage, sync log, "Export log". */
@Composable
fun DiagnosticsDialog(node: KtNode, onClose: () -> Unit) {
    val text = remember(node) { diagnosticsText(node.diagnostics()) }
    val clipboard = LocalClipboardManager.current
    var copied by remember { mutableStateOf(false) }
    AlertDialog(
        onDismissRequest = onClose,
        confirmButton = {
            TextButton(onClick = { clipboard.setText(AnnotatedString(text)); copied = true }, modifier = Modifier.testTag("exportLog")) {
                Text(if (copied) tr("Copied") else tr("Export log"))
            }
        },
        dismissButton = { TextButton(onClick = onClose) { Text(tr("Close")) } },
        title = { Text(tr("Diagnostics")) },
        text = {
            Column(Modifier.heightIn(max = 480.dp).verticalScroll(rememberScrollState())) {
                Text(text, style = MaterialTheme.typography.bodySmall, modifier = Modifier.testTag("diagnostics"))
            }
        },
    )
}
