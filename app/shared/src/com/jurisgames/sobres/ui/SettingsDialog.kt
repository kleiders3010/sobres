package com.jurisgames.sobres.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import com.jurisgames.sobres.core.KtNode
import com.jurisgames.sobres.core.Settings

private val helpLabels = mapOf(
    "wifi_and_charging" to "On Wi-Fi while charging (and on Wi-Fi while open)",
    "wifi_while_open" to "On Wi-Fi while the app is open",
    "on" to "On",
    "off" to "Off",
)

/** The settings (DESIGN settings table). Settings this device doesn't have aren't shown. */
@Composable
fun SettingsDialog(node: KtNode, onSaved: (Settings) -> Unit, onClose: () -> Unit) {
    var s by remember(node) { mutableStateOf(node.settings()) }
    var error by remember { mutableStateOf("") }
    val version = remember(node) { node.diagnostics().version }
    var taps by remember { mutableStateOf(0) }
    var diagnostics by remember { mutableStateOf(false) }
    if (diagnostics) DiagnosticsDialog(node, onClose = { diagnostics = false })
    var feedback by remember { mutableStateOf(false) }
    var thanked by remember { mutableStateOf(false) }
    if (feedback) FeedbackDialog(node, onSent = { feedback = false; thanked = true }, onClose = { feedback = false })
    AlertDialog(
        onDismissRequest = onClose,
        confirmButton = {
            TextButton(onClick = {
                try {
                    node.setSettings(s)
                    onSaved(s)
                    onClose()
                } catch (e: Exception) {
                    error = e.message ?: tr("Couldn't save")
                }
            }, modifier = Modifier.testTag("settingsSave")) { Text(tr("Save")) }
        },
        dismissButton = { TextButton(onClick = onClose) { Text(tr("Cancel")) } },
        title = { Text(tr("Settings")) },
        text = {
            Column(Modifier.verticalScroll(rememberScrollState())) {
                SwitchRow(tr("Sync in the background"), s.syncInBackground, "set-background") { s = s.copy(syncInBackground = it) }
                SwitchRow(tr("Notify me when others change a list"), s.notifyChanges, "set-notify") { s = s.copy(notifyChanges = it) }
                Text(tr("Help others sync"), style = MaterialTheme.typography.titleSmall)
                for (opt in s.helpOptions) {
                    Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
                        RadioButton(s.helpOthers == opt, { s = s.copy(helpOthers = opt) }, modifier = Modifier.testTag("help-$opt"))
                        Text(tr(helpLabels[opt] ?: opt))
                    }
                }
                s.relayForOthers?.let { v -> SwitchRow(tr("Relay connections for others"), v, "set-relay") { s = s.copy(relayForOthers = it) } }
                s.startWithComputer?.let { v -> SwitchRow(tr("Start with computer"), v, "set-autostart") { s = s.copy(startWithComputer = it) } }
                Text(tr("Appearance"), style = MaterialTheme.typography.titleSmall)
                // Material You: offered where the platform has it (Android 12+).
                val dynamic = s.dynamicColor
                if (dynamic != null && LocalDynamicAvailable.current) SwitchRow(tr("Use wallpaper colors"), dynamic, "set-dynamic") { s = s.copy(dynamicColor = it) }
                if (dynamic != true || !LocalDynamicAvailable.current) {
                    for (t in com.jurisgames.sobres.core.themes()) {
                        Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
                            RadioButton(s.theme == t.id, { s = s.copy(theme = t.id) }, modifier = Modifier.testTag("theme-" + t.id))
                            Text(tr(t.name))
                        }
                    }
                }
                if (error.isNotEmpty()) Text(error, modifier = Modifier.testTag("settingsError"))
                if (thanked) Text(tr("Thanks! Your message was sent."), style = MaterialTheme.typography.bodySmall, modifier = Modifier.testTag("feedbackThanks"))
                Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
                    // Hidden diagnostics (DESIGN §2): tap the version seven times.
                    Text(
                        tr("Version {v}", "v" to version),
                        style = MaterialTheme.typography.bodySmall,
                        modifier = Modifier.weight(1f).testTag("version").clickable { if (++taps >= 7) { taps = 0; diagnostics = true } },
                    )
                    // a problem or an idea: to the person who makes the app (with the diagnostics, if the tick stays on)
                    TextButton(onClick = { feedback = true }, modifier = Modifier.testTag("feedbackOpen")) { Text(tr("Send feedback")) }
                }
            }
        },
    )
}

@Composable
private fun SwitchRow(label: String, value: Boolean, tag: String, onChange: (Boolean) -> Unit) {
    Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
        Text(label, modifier = Modifier.weight(1f))
        Switch(value, onChange, modifier = Modifier.testTag(tag))
    }
}
