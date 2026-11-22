package com.jurisgames.sobres.desktop

import com.jurisgames.sobres.ui.tr
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.painter.Painter
import androidx.compose.ui.window.Tray
import androidx.compose.ui.window.Window
import androidx.compose.ui.window.application
import com.jurisgames.sobres.core.KtNode
import com.jurisgames.sobres.core.NodeOptions
import com.jurisgames.sobres.core.Platform
import com.jurisgames.sobres.ui.SobresApp
import java.awt.FileDialog
import java.awt.Frame
import java.io.File

/**
 * Desktop app (Windows, Linux). Data lives in the platform's app-data folder; the identity
 * next to it but outside anything backed up. Paths can be overridden for tests:
 * `-Dkt.home=DIR -Dkt.worker=PATH -Dkt.p2p=false -Dkt.relays=ws://...`.
 * A backup file given as `backup` is restored (first launch, nothing made yet).
 */
fun openNode(backup: ByteArray? = null, password: String? = null): KtNode {
    val base = System.getProperty("kt.home")?.let { File(it) } ?: run {
        val appData = System.getenv("APPDATA") ?: (System.getProperty("user.home") + "/.local/share")
        File(appData, "JurisSobres")
    }
    val identity = File(base, "identity").apply { mkdirs() }
    val data = File(base, "data").apply { mkdirs() }
    val worker = System.getProperty("kt.worker") ?: File(System.getProperty("kt.bin", "."), if (isWindows()) "kt-import-worker.exe" else "kt-import-worker").absolutePath
    val relays = System.getProperty("kt.relays")?.split(",")?.filter { it.isNotBlank() } ?: emptyList()
    val p2p = System.getProperty("kt.p2p", "true").toBoolean()
    val bind = System.getProperty("kt.bind", "")
    // Daily sealed backup (the folder choice prompt comes with the polished UI, DESIGN §11).
    val backups = System.getProperty("kt.backups") ?: File(base, "backups").absolutePath
    val name = System.getProperty("kt.deviceName") ?: System.getenv("COMPUTERNAME") ?: runCatching { java.net.InetAddress.getLocalHost().hostName }.getOrDefault("")
    val options = NodeOptions(Platform.DESKTOP, relays, p2p, p2p, bind, name, backups)
    return if (backup == null) KtNode.open(identity.absolutePath, data.absolutePath, worker, options)
    else KtNode.openRestoring(identity.absolutePath, data.absolutePath, worker, options, backup, password)
}

/** Replace a fresh node with one restored from `backup` (falls back to the old one on failure). */
fun restoreNode(old: KtNode, backup: ByteArray, password: String? = null): Result<KtNode> {
    old.stopLive()
    old.close()
    return runCatching { openNode(backup, password) }
}

private fun isWindows() = System.getProperty("os.name").lowercase().contains("win")

/** The tray icon: a plain check-mark disc (drawn, no image files). */
private object TrayIcon : Painter() {
    override val intrinsicSize = Size(64f, 64f)
    override fun DrawScope.onDraw() {
        drawCircle(Color(0xFF3F6FD8))
        val w = size.width
        drawLine(Color.White, Offset(w * 0.28f, w * 0.52f), Offset(w * 0.44f, w * 0.68f), strokeWidth = w * 0.1f)
        drawLine(Color.White, Offset(w * 0.44f, w * 0.68f), Offset(w * 0.74f, w * 0.34f), strokeWidth = w * 0.1f)
    }
}

/**
 * The desktop app lives in the tray (DESIGN §8: it stays connected and starts with the
 * computer). Closing the window keeps it syncing; Quit ends it. Started at login with
 * `--background`, it opens straight into the tray.
 */
/** This build's version code (the update manifest compares against it): the same number as the Android build
 * (app/android/build.gradle.kts versionCode), checked by VersionCodeTest. */
const val VERSION_CODE = 3uL

fun main(args: Array<String>) {
    // Packaged app: the Rust library and the import worker ship in its resources folder.
    System.getProperty("compose.application.resources.dir")?.let { res ->
        if (System.getProperty("jna.library.path") == null) System.setProperty("jna.library.path", res)
        if (System.getProperty("kt.bin") == null) System.setProperty("kt.bin", res)
    }
    val background = Autostart.BACKGROUND_ARG in args
    val first = openNode()
    Autostart.apply(enabled = first.settings().startWithComputer ?: true) // "Start with computer" (on by default)
    first.startLive()
    application {
        var node by remember { mutableStateOf(first) }
        fun restoreInto(bytes: ByteArray, password: String?) {
            node = restoreNode(node, bytes, password).getOrElse { e ->
                System.err.println("restore failed: ${e.message}") // prototype: no error UI yet
                openNode()
            }
            node.startLive()
        }
        var visible by remember { mutableStateOf(!background) }
        val trayState = androidx.compose.ui.window.rememberTrayState()
        // Reminders (§12): the tray app shows them when they're due.
        androidx.compose.runtime.LaunchedEffect(Unit) {
            val shown = mutableSetOf<String>()
            while (true) {
                val now = System.currentTimeMillis()
                for (r in node.upcomingReminders()) {
                    val key = r.listId + "|" + r.taskId + "|" + r.atMs
                    if (r.atMs.toLong() <= now && shown.add(key)) {
                        trayState.sendNotification(androidx.compose.ui.window.Notification(r.title, r.listName, androidx.compose.ui.window.Notification.Type.Info))
                    }
                }
                kotlinx.coroutines.delay(30_000)
            }
        }
        var update by remember { mutableStateOf<com.jurisgames.sobres.core.UpdateItem?>(null) }
        androidx.compose.runtime.LaunchedEffect(Unit) {
            val channel = if (System.getProperty("os.name").lowercase().contains("win")) "windows" else "linux"
            update = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { runCatching { node.checkUpdate(VERSION_CODE, channel) }.getOrNull() }
        }
        // Quitting: gone from view at once; unsent changes and invites go out (10 s at most),
        // then the process ends. Never a full sync round on the UI thread.
        var quitting by remember { mutableStateOf(false) }
        fun quit() {
            if (quitting) return
            quitting = true
            visible = false
            Thread {
                node.stopLive()
                node.flush(10_000uL)
                java.awt.EventQueue.invokeLater { exitApplication() }
            }.start()
        }
        if (!quitting) Tray(
            state = trayState,
            icon = TrayIcon,
            tooltip = "Sobres",
            onAction = { visible = true },
            menu = {
                Item(tr("Open Sobres"), onClick = { visible = true })
                Item(tr("Sync now"), onClick = { Thread { node.syncNow() }.start() })
                Separator()
                Item(tr("Quit"), onClick = { quit() })
            },
        )
        // Closing the window: stay in the tray and keep syncing, unless background sync is off.
        Window(onCloseRequest = {
            if (node.settings().syncInBackground) visible = false else quit()
        }, visible = visible, title = "Sobres") {
            val required = remember(node) {
                val channel = if (System.getProperty("os.name").lowercase().contains("win")) "windows" else "linux"
                runCatching { node.requiredUpdate(VERSION_CODE, channel) }.getOrNull()
            }
            SobresApp(node, required = required, decodeImage = Images::decode, onPickImage = { done ->
                val dialog = FileDialog(null as Frame?, tr("Add an image"), FileDialog.LOAD).apply {
                    setFilenameFilter { _, name -> name.lowercase().substringAfterLast('.') in setOf("jpg", "jpeg", "png", "gif", "bmp", "webp") }
                    isVisible = true
                }
                dialog.file?.let { Images.compress(File(dialog.directory, it)) }?.let(done)
            }, update = update, onUpdate = { link ->
                runCatching { java.awt.Desktop.getDesktop().browse(java.net.URI(link)) }
            }, onSettings = { Autostart.apply(it.startWithComputer ?: true) }, onShare = { link ->
                // Desktop: onto the clipboard, ready to paste into any chat.
                java.awt.Toolkit.getDefaultToolkit().systemClipboard.setContents(java.awt.datatransfer.StringSelection(link), null)
            }, onRestore = {
                val dialog = FileDialog(null as Frame?, tr("Restore from a backup file"), FileDialog.LOAD).apply { isVisible = true }
                val file = dialog.file?.let { File(dialog.directory, it) }
                if (file != null) {
                    val bytes = file.readBytes()
                    // A password-locked backup: the app asks for its password first (audit T11).
                    if (com.jurisgames.sobres.core.backupNeedsPassword(bytes)) com.jurisgames.sobres.ui.PendingRestore.bytes = bytes
                    else restoreInto(bytes, null)
                }
            }, onRestoreWith = { bytes, pw -> restoreInto(bytes, pw) }, onExport = { l ->
                // The activity as a spreadsheet file (one line per envelope part).
                val dialog = FileDialog(null as Frame?, tr("Export {name}", "name" to l.name), FileDialog.SAVE).apply { file = "${com.jurisgames.sobres.core.exportFileStem(l.name)}.csv"; isVisible = true }
                dialog.file?.let { File(dialog.directory, it) }?.writeText(node.exportCsv(l.id))
            })
        }
    }
}
