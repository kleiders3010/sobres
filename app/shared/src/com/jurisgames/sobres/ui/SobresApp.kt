package com.jurisgames.sobres.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Checkbox
import androidx.compose.material3.DrawerValue
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalDrawerSheet
import androidx.compose.material3.ModalNavigationDrawer
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarDuration
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.SnackbarResult
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.VerticalDivider
import androidx.compose.material3.rememberDrawerState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.foundation.layout.widthIn
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.isCtrlPressed
import androidx.compose.ui.input.key.isShiftPressed
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.jurisgames.sobres.core.InviteInfo
import com.jurisgames.sobres.core.KtListener
import com.jurisgames.sobres.core.KtNode
import com.jurisgames.sobres.core.ListItem
import com.jurisgames.sobres.core.accentColor
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import java.io.File

/**
 * The app: the last budget used, its envelopes, ready to record a purchase (docs/UI-SPEC.md).
 * Every "list" of the shared core is a budget. Budgets are a sidebar on wide screens and chips
 * on phones; everything else is in the budget's ⋮ menu or the ☰ menu.
 */
@Composable
fun SobresApp(
    node: KtNode,
    onRestore: (() -> Unit)? = null,
    onExport: ((ListItem) -> Unit)? = null,
    onShare: ((String) -> Unit)? = null,
    openedCode: String? = null,
    onSettings: ((com.jurisgames.sobres.core.Settings) -> Unit)? = null,
    onScan: ((onResult: (String) -> Unit) -> Unit)? = null,
    onImport: (() -> Unit)? = null,
    /** U-S7: save every budget as a backup file the person keeps (the format Restore reads). */
    onSaveBackup: (() -> Unit)? = null,
    /** A budget was just shared or joined (Android asks for notifications then, U-S2). */
    onShared: (() -> Unit)? = null,
    backHandler: (@Composable (Boolean, () -> Unit) -> Unit)? = null,
    update: com.jurisgames.sobres.core.UpdateItem? = null,
    onUpdate: ((String) -> Unit)? = null,
    /** Pick an image (gallery / file) and hand over its compressed bytes. */
    onPickImage: ((onPicked: (ByteArray) -> Unit) -> Unit)? = null,
    /** Turn image bytes into a bitmap (per platform). */
    decodeImage: ((ByteArray) -> androidx.compose.ui.graphics.ImageBitmap?)? = null,
    /** Why this build must update (signed operator manifest only, never peer data). */
    required: String? = null,
    /** Android 12+: wallpaper colors (Material You), for light or dark. */
    dynamicScheme: ((Boolean) -> androidx.compose.material3.ColorScheme?)? = null,
    /** Print a page (the list's name, the page as HTML): Android's print service, the desktop's print dialog. */
    onPrint: ((String, String) -> Unit)? = null,
    /** Restore a password-locked backup (its bytes, the checked password). */
    onRestoreWith: ((ByteArray, String) -> Unit)? = null,
) {
    // Bumped whenever the node reports a change (another device, membership, compaction).
    var version by remember { mutableIntStateOf(0) }
    DisposableEffect(node) {
        node.setListener(object : KtListener {
            override fun onChange() {
                version++
            }
        })
        onDispose { }
    }
    var restoring by remember { mutableStateOf<Pair<ByteArray, String>?>(null) }
    AppTheme(node, version, dynamicScheme) {
        CompositionLocalProvider(LocalImageDecoder provides decodeImage) {
            Surface(Modifier.fillMaxSize()) {
                // While a locked backup is restored the old node gets closed: nothing on screen may still read it.
                if (restoring == null) Home(
                    Env(node, version, { version++ }, onRestore, onExport, onShare, onSettings, onScan, onImport, update, onUpdate, onPickImage, required, onPrint, onSaveBackup, onShared, backHandler),
                    openedCode,
                )
                // A locked backup waits for its password (picked, or restored by Android onto this phone).
                if (onRestoreWith != null) {
                    LaunchedEffect(node) { node.waitingBackup()?.let { PendingRestore.bytes = it } }
                    PendingRestore.bytes?.let { bytes ->
                        RestorePasswordDialog(bytes, onRestore = { pw -> PendingRestore.bytes = null; restoring = bytes to pw }, onCancel = { PendingRestore.bytes = null })
                    }
                    LaunchedEffect(restoring) {
                        val (bytes, pw) = restoring ?: return@LaunchedEffect
                        androidx.compose.runtime.withFrameNanos { } // the screen above is gone first
                        try { onRestoreWith(bytes, pw) } finally { restoring = null }
                    }
                }
            }
        }
    }
}

/** What every part of the screen needs. */
class Env(
    val node: KtNode,
    val version: Int,
    val changed: () -> Unit,
    val onRestore: (() -> Unit)?,
    val onExport: ((ListItem) -> Unit)?,
    val onShare: ((String) -> Unit)?,
    val onSettings: ((com.jurisgames.sobres.core.Settings) -> Unit)?,
    val onScan: ((onResult: (String) -> Unit) -> Unit)?,
    val onImport: (() -> Unit)?,
    val update: com.jurisgames.sobres.core.UpdateItem?,
    val onUpdate: ((String) -> Unit)?,
    val onPickImage: ((onPicked: (ByteArray) -> Unit) -> Unit)?,
    val required: String?,
    val onPrint: ((String, String) -> Unit)? = null,
    val onSaveBackup: (() -> Unit)? = null,
    val onShared: (() -> Unit)? = null,
    /** Android's Back inside the app (a screen or tab steps back before the app closes; QR U-Q1 / Tasks U-T18). */
    val backHandler: (@Composable (Boolean, () -> Unit) -> Unit)? = null,
)

/** The list shown last, per device (a plain file next to the app's data). */
private fun lastListFile(node: KtNode) = File(node.dataDir(), "ui-last-list")

private enum class Dialog { Settings, Join, LinkThis, LinkCode, NewList, Rename, Style, Undo, Feedback }

@Composable
private fun Home(env: Env, openedCode: String?) {
    val node = env.node
    val scope = rememberCoroutineScope()
    val snackbar = remember { SnackbarHostState() }
    // by name only: sharing a budget moves it into a group of its own, and that must not move its tab
    val lists = remember(node, env.version) { node.lists().sortedWith(compareBy(String.CASE_INSENSITIVE_ORDER) { it.name }) }
    var currentId by remember { mutableStateOf(runCatching { lastListFile(node).readText().trim() }.getOrDefault("")) }
    val current = lists.find { it.id == currentId } ?: lists.firstOrNull()
    fun open(l: ListItem) {
        currentId = l.id
        runCatching { lastListFile(node).writeText(l.id) }
    }
    val opened = openedCode?.takeIf { it.isNotBlank() }
    var dialog by remember(opened) {
        mutableStateOf(if (opened == null) null else if (opened.contains("/link")) Dialog.LinkThis else Dialog.Join)
    }
    var invite by remember { mutableStateOf<Pair<String, InviteInfo>?>(null) }

    fun share(l: ListItem) {
        runCatching {
            val shared = node.shareList(l.id)
            open(shared)
            env.changed()
            invite = shared.name to node.invite(shared.groupId, 30u)
            env.onShared?.invoke()
        }.onFailure { e -> scope.launch { snackbar.showSnackbar(userError(e, tr("Couldn't share"))) } }
    }

    var confirmDelete by remember { mutableStateOf<ListItem?>(null) }
    val keyAdd = remember { mutableStateOf<String?>(null) }

    fun deleteList(l: ListItem, confirmed: Boolean = false) {
        // A shared list goes for everyone in it: the message says so (cold UX review U-T19).
        val shared = node.groups().find { it.id == l.groupId }?.personal == false
        // U-S14: a shared budget is the partner's history too: one confirm that names who loses it.
        if (shared && !confirmed) { confirmDelete = l; return }
        runCatching { node.deleteList(l.id) }
        env.changed()
        scope.launch {
            val msg = if (shared) tr("Deleted {name} for everyone in it", "name" to l.name) else tr("Deleted {name}", "name" to l.name)
            val r = snackbar.showSnackbar(msg, actionLabel = tr("Undo"), duration = SnackbarDuration.Long)
            if (r == SnackbarResult.ActionPerformed) {
                runCatching { node.restoreList(l.groupId, l.id) }
                open(l)
                env.changed()
            }
        }
    }

    val menu = MenuActions(
        join = { dialog = Dialog.Join },
        linkThis = { dialog = Dialog.LinkThis },
        linkCode = { dialog = Dialog.LinkCode },
        settings = { dialog = Dialog.Settings },
        feedback = { dialog = Dialog.Feedback },
    )
    val listMenu = ListActions(
        share = { current?.let(::share) },
        rename = { dialog = Dialog.Rename },
        style = { dialog = Dialog.Style },
        undo = { dialog = Dialog.Undo },
        delete = { current?.let(::deleteList) },
    )

    val keys = Modifier.onPreviewKeyEvent { e ->
        if (e.type != KeyEventType.KeyDown || !e.isCtrlPressed) return@onPreviewKeyEvent false
        // U-S16: Ctrl+N a new expense, Ctrl+Shift+N money in, Ctrl+, Settings, Ctrl+Z the Undo on screen.
        when (e.key) {
            Key.N -> { if (current != null && dialog == null) keyAdd.value = if (e.isShiftPressed) "inc" else "exp"; return@onPreviewKeyEvent current != null }
            Key.Comma -> { dialog = Dialog.Settings; return@onPreviewKeyEvent true }
            Key.Z -> return@onPreviewKeyEvent snackbar.currentSnackbarData?.takeIf { it.visuals.actionLabel != null }?.let { it.performAction(); true } ?: false
            else -> {}
        }
        val digits = listOf(Key.One, Key.Two, Key.Three, Key.Four, Key.Five, Key.Six, Key.Seven, Key.Eight, Key.Nine)
        val i = digits.indexOf(e.key)
        if (i < 0) return@onPreviewKeyEvent false
        lists.getOrNull(i)?.let(::open)
        true
    }

    BoxWithConstraints(Modifier.fillMaxSize().then(keys)) {
        val wide = maxWidth >= 720.dp
        if (wide) {
            Scaffold(snackbarHost = { SnackbarHost(snackbar, Modifier.padding(bottom = if (current != null) 64.dp else 0.dp)) }) { pad ->
                Row(Modifier.fillMaxSize().padding(pad).consumeWindowInsets(pad).imePadding()) {
                    Column(Modifier.width(280.dp).fillMaxHeight().background(MaterialTheme.colorScheme.surfaceContainer)) {
                        Column(Modifier.weight(1f).verticalScroll(rememberScrollState()).padding(vertical = 12.dp)) {
                            Text(tr("Budgets"), style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(horizontal = 20.dp, vertical = 8.dp))
                            for (l in lists) SidebarRow(l, l.id == current?.id) { open(l) }
                            SidebarRow(null, false, tr("New budget")) { dialog = Dialog.NewList }
                        }
                        HorizontalDivider()
                        MenuPanel(env, menu, compact = true)
                    }
                    VerticalDivider()
                    // a wide window keeps the budget at a readable width, centred (Ciclo review U-C25, APP-RULES 168)
                    Box(Modifier.weight(1f).fillMaxHeight(), contentAlignment = Alignment.TopCenter) {
                        Column(Modifier.widthIn(max = 760.dp).fillMaxHeight()) {
                            Banners(env)
                            if (current != null) BudgetPane(env, current, onMenu = null, listMenu = listMenu, snackbar = snackbar, keyAdd = keyAdd)
                            else FirstBudget(env, onLinkThis = menu.linkThis, onJoin = menu.join, onMenu = null, open = ::open)
                        }
                    }
                }
            }
        } else {
            val drawer = rememberDrawerState(DrawerValue.Closed)
            ModalNavigationDrawer(drawerState = drawer, drawerContent = {
                ModalDrawerSheet(Modifier.width(300.dp)) {
                    MenuPanel(env, MenuActions(
                        join = { scope.launch { drawer.close() }; menu.join() },
                        linkThis = { scope.launch { drawer.close() }; menu.linkThis() },
                        linkCode = { scope.launch { drawer.close() }; menu.linkCode() },
                        settings = { scope.launch { drawer.close() }; menu.settings() },
                        feedback = { scope.launch { drawer.close() }; menu.feedback() },
                    ), compact = false)
                }
            }) {
                Scaffold(snackbarHost = { SnackbarHost(snackbar, Modifier.padding(bottom = if (current != null) 64.dp else 0.dp)) }) { pad ->
                    Column(Modifier.fillMaxSize().padding(pad).consumeWindowInsets(pad).imePadding()) {
                        Banners(env)
                        if (current != null) {
                            BudgetPane(env, current, onMenu = { scope.launch { drawer.open() } }, listMenu = listMenu, snackbar = snackbar, keyAdd = keyAdd) {
                                if (lists.size > 1) BudgetChips(lists, current, ::open) { dialog = Dialog.NewList }
                            }
                        } else {
                            FirstBudget(env, onLinkThis = menu.linkThis, onJoin = menu.join, onMenu = { scope.launch { drawer.open() } }, open = ::open)
                        }
                    }
                }
            }
        }
    }

    when (dialog) {
        Dialog.Settings -> SettingsDialog(node, onSaved = { env.onSettings?.invoke(it); env.changed() }, onClose = { dialog = null })
        Dialog.Join -> JoinDialog(env, opened?.takeIf { !it.contains("/link") }, onJoined = { name ->
            env.onShared?.invoke()
            node.lists().lastOrNull { it.groupName == name }?.let(::open)
        }, onClose = { dialog = null })
        // A device link never comes prefilled from a link: linking hands over every budget, so the code is typed or
        // scanned here, from the other device's own screen (safety audit round 4, T6).
        Dialog.LinkThis -> LinkThisDialog(env, opened?.contains("/link") == true, scope) { dialog = null }
        Dialog.LinkCode -> runCatching { node.linkCode() }.getOrNull()?.let { info ->
            CodeDialog(tr("Link another device"), tr("On your other device, open ☰ › Link to my other device, then enter this code or scan it."), info, env.onShare) { dialog = null }
        }
        Dialog.NewList -> NewListDialog(env, onCreated = { open(it); dialog = null }, onClose = { dialog = null })
        Dialog.Rename -> current?.let { l -> RenameDialog(l.name, onDone = { n -> runCatching { node.renameList(l.id, n) }.onSuccess { dialog = null }.onFailure { e -> scope.launch { snackbar.showSnackbar(userError(e, tr("Couldn't save. Try again."))) } }; env.changed() }, onClose = { dialog = null }) }
        Dialog.Style -> current?.let { l -> StyleDialog(node, l, onDone = env.changed, onClose = { dialog = null }) }
        Dialog.Undo -> current?.let { l -> UndoDialog(node, l, onDone = env.changed, onClose = { dialog = null }) }
        Dialog.Feedback -> FeedbackDialog(node, onSent = { dialog = null; scope.launch { snackbar.showSnackbar(tr("Thanks! Your message was sent.")) } }, onClose = { dialog = null })
        null -> {}
    }
    confirmDelete?.let { l ->
        val others = remember(l.id) { runCatching { node.people(l.id) }.getOrDefault(emptyList()).filter { !it.isMe }.map { it.name.ifBlank { tr("Someone") } }.distinct() }
        AlertDialog(
            onDismissRequest = { confirmDelete = null },
            title = { Text(tr("Delete {name} for everyone?", "name" to l.name)) },
            text = { Text(if (others.isEmpty()) tr("It is shared: it goes from every device that joined it, with all its entries.")
                else tr("It goes from {people}'s devices too, with all its entries.", "people" to others.joinToString(", "))) },
            confirmButton = { TextButton(onClick = { confirmDelete = null; deleteList(l, confirmed = true) }, modifier = Modifier.testTag("deleteConfirm")) {
                Text(tr("Delete for everyone"), color = MaterialTheme.colorScheme.error) } },
            dismissButton = { TextButton(onClick = { confirmDelete = null }, modifier = Modifier.testTag("deleteCancel")) { Text(tr("Cancel")) } },
        )
    }
    invite?.let { (name, info) -> CodeDialog(tr("Invite to {name}", "name" to name), tr("Anyone with this link or code can open this budget and join, until it expires. Share it only with the people you invite."), info, env.onShare, node) { invite = null } }
}

class MenuActions(val join: () -> Unit, val linkThis: () -> Unit, val linkCode: () -> Unit, val settings: () -> Unit, val feedback: () -> Unit)
class ListActions(val share: () -> Unit, val rename: () -> Unit, val style: () -> Unit, val undo: () -> Unit, val delete: () -> Unit)

@Composable
private fun SidebarRow(l: ListItem?, selected: Boolean, label: String? = null, onClick: () -> Unit) {
    val bg = if (selected) MaterialTheme.colorScheme.secondaryContainer else androidx.compose.ui.graphics.Color.Transparent
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 8.dp).background(bg, RoundedCornerShape(24.dp)).clickable(onClick = onClick)
            .padding(horizontal = 12.dp, vertical = 10.dp).testTag(if (l != null) "list-" + l.name else "newList"),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (l != null) ListBadge(l, Modifier.padding(end = 10.dp)) else Icon(Ic.Add, null, Modifier.padding(end = 10.dp).size(18.dp))
        Text(l?.name ?: label.orEmpty(), maxLines = 1, overflow = TextOverflow.Ellipsis, fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal)
    }
}

/** The other budgets, as chips under the title (phones, when there is more than one). */
@Composable
private fun BudgetChips(lists: List<ListItem>, current: ListItem?, open: (ListItem) -> Unit, newList: () -> Unit) {
    Row(Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = 12.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        for (l in lists) {
            FilterChip(l.id == current?.id, { open(l) }, label = { Text(l.name, maxLines = 1) }, leadingIcon = { ListBadge(l) }, modifier = Modifier.testTag("list-" + l.name))
        }
        FilterChip(false, newList, label = { Text(tr("New budget")) }, leadingIcon = { Icon(Ic.Add, null, Modifier.size(18.dp)) }, modifier = Modifier.testTag("newList"))
    }
}

/** The ☰ menu: who you are, joining, devices, files, settings. */
@Composable
private fun MenuPanel(env: Env, a: MenuActions, compact: Boolean) {
    val node = env.node
    var name by remember(node, env.version) { mutableStateOf(node.displayName()) }
    val devices = remember(node, env.version) { node.devices() }
    val isLinking = remember(node, env.version) { node.isLinking() }
    Column(Modifier.padding(vertical = 8.dp)) {
        if (!compact) Text("Sobres", style = MaterialTheme.typography.titleLarge, modifier = Modifier.padding(start = 20.dp, top = 12.dp, bottom = 8.dp))
        OutlinedTextField(
            name, { name = it; runCatching { node.setDisplayName(it.trim()) } },
            label = { Text(tr("Your name")) }, singleLine = true,
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp).testTag("name"),
        )
        MenuRow(tr("Join a shared budget"), "join", a.join)
        if (isLinking) {
            Text(tr("Linking: this device joins your other one as soon as it's online"), style = MaterialTheme.typography.bodySmall, modifier = Modifier.padding(horizontal = 20.dp, vertical = 6.dp).testTag("linkingStatus"))
        } else {
            MenuRow(if (devices.size > 1) tr("Your devices ({n}) · Link another device", "n" to devices.size) else tr("Link another device"), "linkDevice", a.linkCode)
            if (devices.size <= 1) MenuRow(tr("Link to my other device"), "linkThis", a.linkThis)
        }
        val pristine = remember(node, env.version) { node.isPristine() }
        // U-S7: a backup can be made, not only restored. Restore stays for a fresh install (it replaces this device's start).
        if (env.onSaveBackup != null && !pristine) MenuRow(tr("Save a backup file"), "saveBackup", env.onSaveBackup)
        if (env.onRestore != null && pristine) MenuRow(tr("Restore from a backup"), "restore", env.onRestore)
        MenuRow(tr("Settings"), "settingsOpen", a.settings)
        // in the menu itself, not only inside Settings (Kleider, 2026-11-21: "Make sure users always have an easy way to give feedback or report bugs on ALL our apps")
        MenuRow(tr("Send feedback"), "feedbackMenu", a.feedback)
    }
}

@Composable
private fun MenuRow(label: String, tag: String, onClick: () -> Unit) {
    Text(label, Modifier.fillMaxWidth().clickable(onClick = onClick).padding(horizontal = 20.dp, vertical = 12.dp).testTag(tag))
}

/** Things that need a moment of attention: updates, a restore, a new device. Shown above the list. */
@Composable
private fun Banners(env: Env) {
    val node = env.node
    @Composable
    fun Banner(text: String, tag: String, vararg actions: Pair<String, () -> Unit>) {
        Row(Modifier.fillMaxWidth().background(MaterialTheme.colorScheme.secondaryContainer).padding(horizontal = 16.dp, vertical = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(text, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f).testTag(tag))
            for ((label, act) in actions) TextButton(onClick = act, modifier = Modifier.testTag("$tag-$label")) { Text(label) }
        }
    }
    val update = env.update
    if (env.required != null) {
        Banner(tr("Please update to keep using shared budgets: {v}", "v" to env.required), "required", *listOfNotNull(env.onUpdate?.let { u -> tr("Update") to { u(update?.link ?: "https://apps.jurisgames.com/") } }).toTypedArray())
    } else if (update != null && env.onUpdate != null) {
        val why = if (node.newerVersionSeen()) tr("A newer version is available to use certain features") else tr("Version {v} is available", "v" to update.versionName)
        Banner(why, "update", tr("Update") to { env.onUpdate.invoke(update.link) })
    }
    for (l in remember(node, env.version) { node.recentLinks() }) {
        Banner(tr("New device linked: {name}", "name" to l.name.ifEmpty { tr("a device") }), "recentLink", tr("Undo") to { node.unlinkDevice(l.deviceId); env.changed() })
    }
    // A device an invite let in, shown to every member with an undo (second-pass audit T2-13, rule 17).
    for (j in remember(node, env.version) { runCatching { node.recentJoins() }.getOrDefault(emptyList()) }) {
        val who = j.name.ifEmpty { tr("A new device") }
        Banner(
            if (j.by.isNotEmpty()) tr("{who} joined {list}, let in by {by}'s invite", "who" to who, "list" to j.listName, "by" to j.by) else tr("{who} joined {list}", "who" to who, "list" to j.listName),
            "recentJoin",
            tr("Remove") to { runCatching { node.removeJoined(j.groupId, j.deviceId) }; env.changed() },
            tr("OK") to { runCatching { node.dismissJoin(j.groupId, j.deviceId) }; env.changed() },
        )
    }
    node.restored()?.let { r ->
        Banner(
            tr("Restored from a backup: {n} of your budgets are back", "n" to r.rehomedLists) + if (r.rejoiningGroups > 0u) tr("; shared budgets open for editing once someone in them is online") else "",
            "restoredInfo",
        )
    }
    val old = remember(node, env.version) { node.replacedDeviceGroups() }
    if (old > 0u) {
        Banner(
            (if (old == 1u) tr("Your previous device can still see {n} shared budget. Lost or stolen?", "n" to old) else tr("Your previous device can still see {n} shared budgets. Lost or stolen?", "n" to old)), "oldDevice",
            tr("Remove it") to { node.removeReplacedDevice(); env.changed() }, tr("Keep it") to { node.keepReplacedDevice(); env.changed() },
        )
    }
}

@Composable
private fun RenameDialog(name: String, onDone: (String) -> Unit, onClose: () -> Unit) {
    var text by remember { mutableStateOf(name) }
    AlertDialog(
        onDismissRequest = onClose,
        confirmButton = { TextButton(onClick = { if (text.isNotBlank()) onDone(text.trim()) }, modifier = Modifier.testTag("renameGo")) { Text(tr("Rename")) } },
        dismissButton = { TextButton(onClick = onClose) { Text(tr("Cancel")) } },
        title = { Text(tr("Rename budget")) },
        text = { OutlinedTextField(text, { text = it }, singleLine = true, modifier = Modifier.testTag("renameText")) },
    )
}

@Composable
private fun NewListDialog(env: Env, onCreated: (ListItem) -> Unit, onClose: () -> Unit) {
    var name by remember { mutableStateOf("") }
    fun create() {
        if (name.isBlank()) return
        val l = runCatching { env.node.createList(null, name.trim()) }.getOrNull() ?: return
        runCatching { env.node.setListStyle(l.id, budgetHue(env.node.lists().size - 1), 1u, null) }
        env.changed()
        onCreated(l)
    }
    AlertDialog(
        onDismissRequest = onClose,
        confirmButton = { TextButton(onClick = ::create, modifier = Modifier.testTag("newListGo")) { Text(tr("Create")) } },
        dismissButton = { TextButton(onClick = onClose) { Text(tr("Cancel")) } },
        title = { Text(tr("New budget")) },
        text = {
            OutlinedTextField(
                name, { name = it }, singleLine = true, placeholder = { Text(tr("Name")) },
                keyboardOptions = androidx.compose.foundation.text.KeyboardOptions(imeAction = androidx.compose.ui.text.input.ImeAction.Done),
                keyboardActions = androidx.compose.foundation.text.KeyboardActions(onDone = { create() }),
                modifier = Modifier.testTag("newListName"),
            )
        },
    )
}

/** U-S11: in a shared budget the point is who spent what, so a person without a name is asked where sharing happens. */
@Composable
internal fun NameAsk(node: KtNode) {
    val had = remember(node) { node.displayName().isNotBlank() }
    if (had) return
    var name by remember { mutableStateOf("") }
    OutlinedTextField(name, { name = it; runCatching { node.setDisplayName(it.trim()) } }, singleLine = true,
        label = { Text(tr("Your name (so the others know who spent what)")) }, modifier = Modifier.fillMaxWidth().testTag("askName"))
}

/** An invite or link code: copy / share the link, the words with Copy, the QR code on request. */
@Composable
private fun CodeDialog(title: String, hint: String, info: InviteInfo, onShare: ((String) -> Unit)?, nameNode: KtNode? = null, onClose: () -> Unit) {
    val clip = androidx.compose.ui.platform.LocalClipboardManager.current
    var qr by remember { mutableStateOf(false) }
    var copied by remember { mutableStateOf("") }
    val until = java.time.format.DateTimeFormatter.ofPattern("d MMM", Lang.locale).format(fromMillis(info.expiresMs.toLong())).let(::shortName)
    AlertDialog(
        onDismissRequest = onClose,
        confirmButton = { TextButton(onClick = onClose, modifier = Modifier.testTag("codeDone")) { Text(tr("Done")) } },
        title = { Text(title) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                nameNode?.let { NameAsk(it) }
                Text(hint, style = MaterialTheme.typography.bodyMedium)
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    androidx.compose.material3.Button(onClick = { clip.setText(androidx.compose.ui.text.AnnotatedString(info.link)); copied = tr("Link copied") }, modifier = Modifier.testTag("copyLink")) { Text(tr("Copy link")) }
                    if (onShare != null) androidx.compose.material3.OutlinedButton(onClick = { onShare(info.link) }, modifier = Modifier.testTag("shareInvite")) { Text(tr("Share")) }
                }
                Text(tr("Or send them this code"), style = MaterialTheme.typography.labelLarge)
                Text(info.words.joinToString(" - "), style = MaterialTheme.typography.titleMedium, modifier = Modifier.testTag("words"))
                Row(verticalAlignment = Alignment.CenterVertically) {
                    TextButton(onClick = { clip.setText(androidx.compose.ui.text.AnnotatedString(info.words.joinToString(" "))); copied = tr("Code copied") }, modifier = Modifier.testTag("copyCode")) { Text(tr("Copy code")) }
                    TextButton(onClick = { qr = !qr }, modifier = Modifier.testTag("showQr")) { Text(if (qr) tr("Hide QR code") else tr("Show QR code")) }
                }
                if (qr) Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) { QrView(info.link) }
                Text((if (copied.isNotEmpty()) "$copied · " else "") + tr("Works until {date}", "date" to until), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        },
    )
}

@Composable
private fun LinkThisDialog(env: Env, fromLink: Boolean, scope: CoroutineScope, onClose: () -> Unit) {
    val node = env.node
    var words by remember { mutableStateOf("") }
    var preview by remember { mutableStateOf<String?>(null) }
    var result by remember { mutableStateOf("") }
    val ownLists = remember(node, env.version) { node.lists().size }
    AlertDialog(
        onDismissRequest = onClose,
        confirmButton = {
            if (preview == null) {
                TextButton(onClick = {
                    result = tr("Looking…")
                    scope.launch {
                        try {
                            val p = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { node.linkPreview(words) }
                            // Fixed words say what is handed over; the other side's own name and summary are shown as
                            // theirs (in quotes), never as the app's wording.
                            preview = tr("Linking gives that device everything: all the budgets on this device, and a place in every budget you share. Only go on if you made this code yourself on your other device.") +
                                "\n" + tr("It calls itself “{name}” ({summary})", "name" to p.accountName, "summary" to trSummary(p.summary)) + when (ownLists) {
                                0 -> "."
                                1 -> tr(". The budget on this device is added to them.")
                                else -> tr(". The {n} budgets on this device are added to them.", "n" to ownLists)
                            }
                            result = ""
                        } catch (e: Exception) {
                            result = userError(e, tr("That code didn't work. Check it on your other device."))
                        }
                    }
                }, modifier = Modifier.testTag("linkFind")) { Text(tr("Next")) }
            } else {
                TextButton(onClick = {
                    scope.launch {
                        result = try {
                            kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { node.linkConfirm(words) }
                            env.changed()
                            tr("Linking: this device joins as soon as your other one is online")
                        } catch (e: Exception) {
                            userError(e, tr("Couldn't link. Try again."))
                        }
                    }
                }, modifier = Modifier.testTag("linkGo")) { Text(tr("Link")) }
            }
        },
        dismissButton = { TextButton(onClick = onClose) { Text(tr("Close")) } },
        title = { Text(tr("Link to my other device")) },
        text = {
            Column {
                if (fromLink) Text(tr("For safety, a device is linked only with the code shown on your other device: type it or scan it here."), modifier = Modifier.testTag("linkFromLink"))
                OutlinedTextField(words, { words = it; preview = null }, label = { Text(tr("Code or link from your other device")) }, modifier = Modifier.testTag("linkThisWords"))
                env.onScan?.let { scan -> TextButton(onClick = { scan { words = it; preview = null } }, modifier = Modifier.testTag("scanLink")) { Text(tr("Scan QR code")) } }
                preview?.let { Text(it, modifier = Modifier.testTag("linkPreview")) }
                if (result.isNotEmpty()) Text(result, modifier = Modifier.testTag("linkResult"))
            }
        },
    )
}
