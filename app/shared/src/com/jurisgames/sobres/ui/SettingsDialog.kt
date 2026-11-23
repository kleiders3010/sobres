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
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.toggleable
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
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

const val SOURCE_URL = "https://github.com/kleiders3010/sobres"
const val PRIVACY_URL = "https://apps.jurisgames.com/privacy/"

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
    var backupPw by remember { mutableStateOf(false) }
    if (backupPw) BackupPasswordDialog(node, onClose = { backupPw = false })
    if (feedback) FeedbackDialog(node, onSent = { feedback = false; thanked = true }, onClose = { feedback = false })
    // Every change applies at once; there is nothing to lose by closing (cold UX review U-T11).
    fun apply(n: Settings) {
        try {
            node.setSettings(n)
            s = n
            error = ""
            onSaved(n)
        } catch (e: Exception) {
            error = userError(e, tr("Couldn't save. Try again."))
        }
    }
    var advanced by remember { mutableStateOf(false) }
    var mode by remember(node) { mutableStateOf(darkMode(node.dataDir())) }
    AlertDialog(
        onDismissRequest = onClose,
        confirmButton = { TextButton(onClick = onClose, modifier = Modifier.testTag("settingsSave")) { Text(tr("Done")) } },
        title = { Text(tr("Settings")) },
        text = {
            Column(Modifier.verticalScroll(rememberScrollState())) {
                SwitchRow(tr("Sync in the background"), s.syncInBackground, "set-background") { apply(s.copy(syncInBackground = it)) }
                SwitchRow(tr("Tell me when someone adds an entry"), s.notifyChanges, "set-notify") { apply(s.copy(notifyChanges = it)) }
                s.startWithComputer?.let { v -> SwitchRow(tr("Start with computer"), v, "set-autostart") { apply(s.copy(startWithComputer = it)) } }
                Text(tr("Light or dark"), style = MaterialTheme.typography.titleSmall)
                for (m in DarkMode.entries) RadioRow(tr(m.label), mode == m, "mode-" + m.key) { mode = m; setDarkMode(node.dataDir(), m); onSaved(s) }
                Text(tr("Appearance"), style = MaterialTheme.typography.titleSmall)
                // Material You: offered where the platform has it (Android 12+).
                val dynamic = s.dynamicColor
                if (dynamic != null && LocalDynamicAvailable.current) SwitchRow(tr("Use wallpaper colors"), dynamic, "set-dynamic") { apply(s.copy(dynamicColor = it)) }
                if (dynamic != true || !LocalDynamicAvailable.current) {
                    for (t in com.jurisgames.sobres.core.themes()) {
                        RadioRow(tr(t.name), s.theme == t.id, "theme-" + t.id) { apply(s.copy(theme = t.id)) }
                    }
                }
                // Network help for others: plain words, folded away (U-T11).
                Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp).clickable { advanced = !advanced }.testTag("advancedOpen")) {
                    Text(tr("Advanced"), style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
                    Text(if (advanced) "\u25B2" else "\u25BC", style = MaterialTheme.typography.bodySmall)
                }
                if (advanced) {
                    Text(tr("Help others sync"), style = MaterialTheme.typography.bodyMedium)
                    Text(tr("Lets this device pass along encrypted updates for other Sobres users. It can't read them. Uses some data and battery."), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    for (opt in s.helpOptions) RadioRow(tr(helpLabels[opt] ?: opt), s.helpOthers == opt, "help-$opt") { apply(s.copy(helpOthers = opt)) }
                    s.relayForOthers?.let { v ->
                        SwitchRow(tr("Relay connections for others"), v, "set-relay") { apply(s.copy(relayForOthers = it)) }
                        Text(tr("When on, other Sobres users whose devices can't reach each other directly can connect through this computer. Everything stays encrypted."), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
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
                Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
                    Text(tr("Backup password"), modifier = Modifier.weight(1f))
                    TextButton(onClick = { backupPw = true }, modifier = Modifier.testTag("backupPwOpen")) { Text(tr(if (remember(node, backupPw) { node.hasBackupPassword() }) "Change it" else "Set one")) }
                }
                // What the app is, with its source and privacy policy one tap away (cold UX review U-T22).
                Text(tr("Free and open source. No ads, no account."), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                val uri = androidx.compose.ui.platform.LocalUriHandler.current
                Row {
                    TextButton(onClick = { runCatching { uri.openUri(SOURCE_URL) } }, modifier = Modifier.testTag("sourceLink")) { Text(tr("Source code")) }
                    TextButton(onClick = { runCatching { uri.openUri(PRIVACY_URL) } }, modifier = Modifier.testTag("privacyLink")) { Text(tr("Privacy")) }
                }
            }
        },
    )
}

@Composable
private fun SwitchRow(label: String, value: Boolean, tag: String, onChange: (Boolean) -> Unit) {
    // The whole row is the switch: label included, 48 dp tall (cold UX review U-T14).
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp).toggleable(value, role = Role.Switch, onValueChange = onChange).testTag(tag),
    ) {
        Text(label, modifier = Modifier.weight(1f))
        Switch(value, null)
    }
}

/** One choice of several; the whole row picks it (U-T14). */
@Composable
private fun RadioRow(label: String, selected: Boolean, tag: String, onPick: () -> Unit) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp).selectable(selected, role = Role.RadioButton, onClick = onPick).testTag(tag),
    ) {
        RadioButton(selected, null)
        Text(label)
    }
}
