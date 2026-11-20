package com.jurisgames.sobres

import com.jurisgames.sobres.ui.tr
import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.enableEdgeToEdge
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.lifecycle.lifecycleScope
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.platform.testTag
import com.jurisgames.sobres.ui.SobresApp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

class MainActivity : ComponentActivity() {
    private val app get() = application as TasksApplication

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge() // the app draws behind the status and navigation bars and pads itself
        super.onCreate(savedInstanceState)
        opened.value = intent?.dataString
        QrScanner.prepare(this) // play: fetch Google's scanner module before anyone taps Scan
        setContent {
            var node by remember { mutableStateOf(app.node) }
            val scope = rememberCoroutineScope()
            // Images on tasks: the system photo picker (no storage permission), shrunk first.
            var onPicked by remember { mutableStateOf<((ByteArray) -> Unit)?>(null) }
            val pickImage = rememberLauncherForActivityResult(ActivityResultContracts.PickVisualMedia()) { uri ->
                val done = onPicked
                onPicked = null
                if (uri != null && done != null) scope.launch {
                    val bytes = withContext(Dispatchers.IO) { Images.compress(this@MainActivity, uri) }
                    if (bytes != null) done(bytes) else Toast.makeText(this@MainActivity, tr("Couldn't read that image"), Toast.LENGTH_LONG).show()
                }
            }
            val importList = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
                if (uri != null) scope.launch {
                    val msg = try {
                        val l = withContext(Dispatchers.IO) { node.importListFile(null, contentResolver.openInputStream(uri)!!.use { it.readBytes() }) }
                        tr("Imported {name}", "name" to l.name)
                    } catch (e: Exception) {
                        tr(e.message ?: tr("Not a list file"))
                    }
                    Toast.makeText(this@MainActivity, msg, Toast.LENGTH_LONG).show()
                }
            }
            val pick = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
                if (uri != null) scope.launch {
                    try {
                        node = withContext(Dispatchers.IO) {
                            val bytes = contentResolver.openInputStream(uri)!!.use { it.readBytes() }
                            app.restoreFrom(bytes)
                        }
                        node.startLive()
                    } catch (e: Exception) {
                        node = app.node
                        Toast.makeText(this@MainActivity, tr(e.message ?: tr("Restore failed")), Toast.LENGTH_LONG).show()
                    }
                }
            }
            SobresApp(node, onPickImage = { done ->
                onPicked = done
                pickImage.launch(androidx.activity.result.PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly))
            }, decodeImage = Images::decode, dynamicScheme = if (Build.VERSION.SDK_INT >= 31) { dark -> if (dark) androidx.compose.material3.dynamicDarkColorScheme(this) else androidx.compose.material3.dynamicLightColorScheme(this) } else null, onShare = { link ->
                startActivity(Intent.createChooser(Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, link), tr("Share invite")))
            }, update = update.value, required = required.value, onUpdate = { link ->
                startActivity(Intent(Intent.ACTION_VIEW, android.net.Uri.parse(link)))
            }, onScan = { QrScanner.scan(this, it) }, openedCode = opened.value, onSettings = { app.applyBackgroundSync(it.syncInBackground) }, onRestore = { pick.launch(arrayOf("*/*")) }, onExport = { l ->
                // The activity as a CSV file, shared to any app (Sheets, email, Drive).
                val dir = java.io.File(cacheDir, "exports").apply { mkdirs() }
                val file = java.io.File(dir, com.jurisgames.sobres.core.exportFileStem(l.name) + ".csv")
                file.writeText(node.exportCsv(l.id))
                val uri = androidx.core.content.FileProvider.getUriForFile(this, "$packageName.files", file)
                val send = Intent(Intent.ACTION_SEND).setType("text/csv")
                    .putExtra(Intent.EXTRA_SUBJECT, l.name)
                    .putExtra(Intent.EXTRA_STREAM, uri)
                    .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
                startActivity(Intent.createChooser(send, tr("Export {name}", "name" to l.name)))
            })
            var blocked by remember { mutableStateOf(BackgroundCheck.shouldExplain(this@MainActivity)) }
            if (blocked) androidx.compose.material3.AlertDialog(
                onDismissRequest = { blocked = false },
                title = { androidx.compose.material3.Text(tr("Updates only while the app is open")) },
                text = { androidx.compose.material3.Text(tr("This phone is stopping Sobres from syncing in the background, so shared budgets only update while the app is open. To fix it, allow background activity for Sobres in its battery settings.")) },
                confirmButton = {
                    androidx.compose.material3.TextButton(onClick = { blocked = false; BackgroundCheck.openSettings(this@MainActivity) }, modifier = androidx.compose.ui.Modifier.testTag("bgSettings")) {
                        androidx.compose.material3.Text(tr("Battery settings"))
                    }
                },
                dismissButton = { androidx.compose.material3.TextButton(onClick = { blocked = false }) { androidx.compose.material3.Text(tr("Not now")) } },
            )
        }
        holdSplash()
    }

    /** The splash stays until the first sync round or ~1.5 s (DESIGN §8), so lists open current. */
    private fun holdSplash() {
        val shownAt = android.os.SystemClock.uptimeMillis()
        val content = findViewById<android.view.View>(android.R.id.content)
        content.viewTreeObserver.addOnPreDrawListener(object : android.view.ViewTreeObserver.OnPreDrawListener {
            override fun onPreDraw(): Boolean {
                val ready = app.node.firstRoundDone() || android.os.SystemClock.uptimeMillis() - shownAt > 1500
                if (ready) content.viewTreeObserver.removeOnPreDrawListener(this)
                return ready
            }
        })
    }

    /** A newer version, if the daily check found one. */
    private val update = mutableStateOf<com.jurisgames.sobres.core.UpdateItem?>(null)

    /** At most once a day, off the main thread. */
    private fun checkForUpdate() {
        val prefs = getSharedPreferences("update", MODE_PRIVATE)
        val now = System.currentTimeMillis()
        if (now - prefs.getLong("checked", 0) < 24 * 3600_000L) return
        val current = packageManager.getPackageInfo(packageName, 0).longVersionCode.toULong()
        lifecycleScope.launch(Dispatchers.IO) {
            // A failed check is just "nothing found this time": tried again next launch.
            val found = runCatching { app.node.checkUpdate(current, Store.UPDATE_CHANNEL) }.getOrElse { return@launch }
            prefs.edit().putLong("checked", now).apply()
            update.value = found
        }
    }

    /** The invite or device link that opened the app, if any. */
    private val opened = mutableStateOf<String?>(null)

    /** Why this build must update (from the signed operator manifest), if it must. */
    private val required = mutableStateOf<String?>(null)

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        opened.value = intent.dataString
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        // The clipboard is readable only with window focus (Android 10+).
        if (hasFocus) checkClipboardOnce()
    }

    /**
     * Install from an invite (DESIGN §7): the join page copies the link before sending
     * people to the store, so the first launch of this install looks for it. The clip's
     * description is checked first, which shows no "pasted" notice; the text is read only if
     * it may be a link, and taken only if it is one of ours. Otherwise, tapping the link
     * again opens the app with it.
     */
    private fun checkClipboardOnce() {
        val marker = java.io.File(noBackupFilesDir, "clipboard_checked")
        if (marker.exists()) return
        marker.writeText("")
        if (opened.value != null) return
        val cm = getSystemService(android.content.ClipboardManager::class.java) ?: return
        val d = cm.primaryClipDescription ?: return
        if (!d.hasMimeType("text/*")) return
        if (Build.VERSION.SDK_INT >= 31 && d.classificationStatus == android.content.ClipDescription.CLASSIFICATION_COMPLETE &&
            d.getConfidenceScore(android.view.textclassifier.TextClassifier.TYPE_URL) < 0.5f
        ) return
        val text = cm.primaryClip?.getItemAt(0)?.coerceToText(this)?.toString()?.trim() ?: return
        if (INVITE_LINK.matches(text)) {
            opened.value = text
            if (Build.VERSION.SDK_INT >= 28) cm.clearPrimaryClip() // the code is a secret; don't leave it around
        }
    }

    private companion object {
        val INVITE_LINK = Regex("https://apps\\.jurisgames\\.com/sobres/(join|link)/?#[^\\s]+")
    }

    override fun onStart() {
        super.onStart()
        app.node.markSeen() // what's on screen needs no notification
        app.reportDeviceState()
        multicast.acquire() // Android drops multicast (LAN beacons) without it
        app.node.startLive() // app open: live on every layer
        askForNotificationsWhenShared()
        checkForUpdate()
        val current = packageManager.getPackageInfo(packageName, 0).longVersionCode.toULong()
        required.value = runCatching { app.node.requiredUpdate(current, Store.UPDATE_CHANNEL) }.getOrNull()
    }

    /** Asked the first time it's relevant: once this device is in a shared group (§12). */
    private val notifyPermission = registerForActivityResult(ActivityResultContracts.RequestPermission()) { }

    private fun askForNotificationsWhenShared() {
        if (Build.VERSION.SDK_INT < 33 || checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED) return
        if (app.node.groups().any { !it.personal }) notifyPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
    }

    /** Held while the app is open, for the same-Wi-Fi beacons (§6). */
    private val multicast by lazy {
        getSystemService(android.net.wifi.WifiManager::class.java).createMulticastLock("sobres-lan").apply { setReferenceCounted(false) }
    }

    override fun onStop() {
        if (multicast.isHeld) multicast.release()
        app.node.stopLive()
        SyncWorker.flushNow(this) // everything is saved; send it without waiting (§8)
        super.onStop()
    }
}
