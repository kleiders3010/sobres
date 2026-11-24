package com.jurisgames.sobres.ui

import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.selection.toggleable
import androidx.compose.ui.semantics.Role
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.layout.widthIn
import androidx.compose.material3.DatePicker
import androidx.compose.material3.DatePickerDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.rememberDatePickerState
import androidx.compose.runtime.saveable.Saver
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.window.DialogProperties
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SnackbarDuration
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.SnackbarResult
import androidx.compose.material3.Switch
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
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.jurisgames.sobres.core.AccountItem
import com.jurisgames.sobres.core.BudgetView
import com.jurisgames.sobres.core.EnvelopeItem
import com.jurisgames.sobres.core.ListItem
import com.jurisgames.sobres.core.PartItem
import com.jurisgames.sobres.core.RepeatItem
import com.jurisgames.sobres.core.TxEdit
import com.jurisgames.sobres.core.TxItem
import com.jurisgames.sobres.core.accentColor
import kotlinx.coroutines.launch
import java.time.LocalDate
import java.time.YearMonth
import java.time.format.DateTimeFormatter

// ---- money and dates -------------------------------------------------------------------------

/** Cents as the person reads them: "1,250.00", with the budget's symbol in front ("$1,250.00"). */
fun money(cents: Long, symbol: String = "", sign: Boolean = false): String {
    val nf = java.text.NumberFormat.getNumberInstance(moneyLocale()).apply { minimumFractionDigits = 2; maximumFractionDigits = 2 }
    val body = symbol + nf.format(kotlin.math.abs(cents) / 100.0)
    return when {
        cents < 0 -> "−$body"
        sign && cents > 0 -> "+$body"
        else -> body
    }
}

/**
 * What was typed as an amount, in cents: "12", "12.5", "12,50", "1,250.00", "1.250,00". The last
 * '.' or ',' followed by one or two digits is the decimal mark; other marks group thousands.
 */
fun parseCents(text: String): Long? {
    val t = text.filter { it.isDigit() || it == '.' || it == ',' || it == '-' }
    if (t.isEmpty() || t.count { it == '-' } > 1 || (t.contains('-') && !t.startsWith('-'))) return null
    val neg = t.startsWith('-')
    val s = t.removePrefix("-")
    val last = maxOf(s.lastIndexOf('.'), s.lastIndexOf(','))
    val (whole, frac) = if (last >= 0 && s.length - last - 1 in 1..2) s.substring(0, last) to s.substring(last + 1) else s to ""
    val w = whole.filter { it.isDigit() }.ifEmpty { if (frac.isEmpty()) return null else "0" }
    if (w.length > 12) return null
    val c = w.toLong() * 100 + (frac.padEnd(2, '0').toLongOrNull() ?: 0)
    return if (neg) -c else c
}

/**
 * Amounts in the person's own marks (U-S8): a Spanish device keeps its country ("1.250,00" in es-AR and es-UY,
 * "1,250.00" in es-MX); day and month names stay [Lang.locale].
 */
fun moneyLocale(): java.util.Locale {
    val d = java.util.Locale.getDefault()
    return if (Lang.spanish) (if (d.language == "es") d else Lang.locale) else Lang.locale
}

/** "0.00" in the person's marks, for an empty amount field. */
fun zeroAmount(): String = money(0L)

/** U-S6: a date is picked on a calendar in the person's own order, never typed as year-month-day. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PickDateDialog(initial: String, onPicked: (String) -> Unit, onClose: () -> Unit) {
    val start = runCatching { LocalDate.parse(initial) }.getOrDefault(LocalDate.now())
    val ds = rememberDatePickerState(initialSelectedDateMillis = start.atStartOfDay().toInstant(java.time.ZoneOffset.UTC).toEpochMilli())
    DatePickerDialog(
        onDismissRequest = onClose,
        confirmButton = {
            TextButton(onClick = {
                ds.selectedDateMillis?.let { onPicked(java.time.Instant.ofEpochMilli(it).atZone(java.time.ZoneOffset.UTC).toLocalDate().toString()) }
                onClose()
            }, modifier = Modifier.testTag("datePickOk")) { Text(tr("OK")) }
        },
        dismissButton = { TextButton(onClick = onClose) { Text(tr("Cancel")) } },
    ) { DatePicker(ds, modifier = Modifier.testTag("datePicker")) }
}

/** The amount as it goes back into a field ("12.50", no grouping). */
fun centsText(c: Long): String = if (c == 0L) "" else "%d.%02d".format(c / 100, kotlin.math.abs(c % 100)).let { if (c < 0 && c / 100 == 0L) "-$it" else it }

fun today(): String = LocalDate.now().toString()

private fun dayLabel(d: String): String {
    val date = runCatching { LocalDate.parse(d) }.getOrNull() ?: return d
    val now = LocalDate.now()
    return when (date) {
        now -> tr("Today")
        now.minusDays(1) -> tr("Yesterday")
        else -> shortName(DateTimeFormatter.ofPattern(if (date.year == now.year) "EEE d MMM" else "d MMM yyyy", Lang.locale).format(date))
    }
}

private fun monthLabel(m: YearMonth): String = DateTimeFormatter.ofPattern("MMMM yyyy", Lang.locale).format(m).replaceFirstChar { it.uppercase() }

/** Red for money that is short, green for money coming in. */
private val Gain = Color(0xFF2E7D32)
private val GainDark = Color(0xFF81C784)

@Composable
private fun gainColor() = if (LocalLook.current.dark) GainDark else Gain

// ---- the budget screen -----------------------------------------------------------------------

enum class Section(val label: String, val tag: String) {
    Envelopes("Envelopes", "secEnvelopes"), Activity("Activity", "secActivity"), Accounts("Accounts", "secAccounts"), Month("Month", "secMonth"),
}

/** What the Add sheet opens on. */
class AddRequest(val kind: String, val edit: TxItem? = null, val envelope: String? = null, val repeat: RepeatItem? = null, val account: String? = null)

/** How often a repeating entry comes back (core: "w", "2w", "m", "y"). */
private val EVERY = listOf("w" to "Every week", "2w" to "Every 2 weeks", "m" to "Every month", "y" to "Every year")

private fun TxItem.toEdit() = TxEdit(kind, date, amount, accountId, toId, payee, note, parts)

/**
 * A budget: title bar, the sections, and the "Spent…" bar that is always there (docs/UI-SPEC.md).
 * [chips] goes under the title bar (phones: the other budgets).
 */
@Composable
@OptIn(ExperimentalLayoutApi::class)
fun BudgetPane(
    env: Env,
    list: ListItem,
    onMenu: (() -> Unit)?,
    listMenu: ListActions,
    snackbar: SnackbarHostState,
    /** Ctrl+N / Ctrl+Shift+N on desktop (U-S16): the kind of entry to open the Add sheet for. */
    keyAdd: androidx.compose.runtime.MutableState<String?>? = null,
    chips: (@Composable () -> Unit)? = null,
) {
    val node = env.node
    val scope = rememberCoroutineScope()
    val look = LocalLook.current
    val accent = argb(accentColor(look.themeId, list.hue, list.vivid, look.dark))
    val groups = remember(node, env.version) { node.groups() }
    val shared = groups.find { it.id == list.groupId }?.personal == false
    val people = remember(node, env.version, list.id) { if (shared) runCatching { node.people(list.id) }.getOrDefault(emptyList()) else emptyList() }
    val day = today()
    val b = remember(node, env.version, list.id, day) { runCatching { node.budget(list.id, day) }.getOrNull() } ?: return
    val activity = remember(node, env.version, list.id) { runCatching { node.budgetActivity(list.id) }.getOrDefault(emptyList()) }
    val repeating = remember(node, env.version, list.id) { runCatching { node.repeating(list.id) }.getOrDefault(emptyList()) }
    // Rent, pay, bills: what repeats is posted when its day comes (on any device; the same entry once).
    androidx.compose.runtime.LaunchedEffect(node, list.id, day, env.version) {
        // Off the main thread (S3-5): a budget with many repeats must not freeze the screen while they are posted.
        val posted = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { runCatching { node.postDue(list.id, day) }.getOrDefault(emptyList()) }
        if (posted.isNotEmpty()) env.changed()
    }
    var section by remember(list.id) { mutableStateOf(Section.Envelopes) }
    env.backHandler?.invoke(section != Section.Envelopes) { section = Section.Envelopes }
    var menuOpen by remember { mutableStateOf(false) }
    // U-S5: an open Add sheet comes back after rotation or Android recreating the app (its fields are saved inside it).
    var add by rememberSaveable(list.id, stateSaver = Saver<AddRequest?, List<String>>(
        save = { r -> r?.let { listOf(it.kind, it.edit?.id.orEmpty(), it.envelope.orEmpty(), it.repeat?.id.orEmpty(), it.account.orEmpty()) } },
        restore = { v -> AddRequest(v[0], activity.find { it.id == v[1] }, v[2].ifEmpty { null }, repeating.find { it.id == v[3] }, v[4].ifEmpty { null }) },
    )) { mutableStateOf<AddRequest?>(null) }
    var fill by remember { mutableStateOf<Long?>(null) }
    var openEnv by remember { mutableStateOf<String?>(null) }
    var editEnv by remember { mutableStateOf<EnvelopeItem?>(null) }
    var newEnv by remember { mutableStateOf(false) }
    var editAcct by remember { mutableStateOf<AccountItem?>(null) }
    var newAcct by remember { mutableStateOf(false) }
    var reconcile by remember { mutableStateOf<String?>(null) }
    var currency by remember { mutableStateOf(false) }
    keyAdd?.value?.let { k -> androidx.compose.runtime.LaunchedEffect(k) { add = AddRequest(k); keyAdd.value = null } }

    /** U-S4: a save the core refused says so, and its dialog stays open. */
    fun couldNotSave(e: Throwable) {
        scope.launch { snackbar.showSnackbar(userError(e, tr("Couldn't save. Try again."))) }
    }

    fun undoable(message: String, undo: () -> Unit) {
        snackbar.currentSnackbarData?.dismiss() // the newest change is the one Undo takes back
        scope.launch {
            if (snackbar.showSnackbar(message, actionLabel = tr("Undo"), duration = SnackbarDuration.Long) == SnackbarResult.ActionPerformed) {
                runCatching(undo).onFailure { System.err.println("undo failed: ${it.message}") }
                env.changed()
            }
        }
    }

    Column(Modifier.fillMaxSize()) {
        Row(Modifier.fillMaxWidth().padding(start = 4.dp, end = 4.dp, top = 8.dp), verticalAlignment = Alignment.CenterVertically) {
            if (onMenu != null) IconButton(onClick = onMenu, modifier = Modifier.testTag("menu")) { Icon(Ic.Menu, tr("Menu")) } else Spacer(Modifier.width(16.dp))
            Text(list.name, style = MaterialTheme.typography.headlineSmall, color = accent, fontWeight = FontWeight.SemiBold, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f).testTag("title"))
            Row(horizontalArrangement = Arrangement.spacedBy((-6).dp)) { for (p in people.filter { !it.isMe }.take(4)) Avatar(p) }
            IconButton(onClick = listMenu.share, modifier = Modifier.testTag("share")) { Icon(Ic.PersonAdd, if (shared) tr("Invite") else tr("Share")) }
            Box {
                IconButton(onClick = { menuOpen = true }, modifier = Modifier.testTag("listMenu")) { Icon(Ic.More, tr("More")) }
                DropdownMenu(menuOpen, { menuOpen = false }) {
                    @Composable
                    fun item(label: String, tag: String, act: () -> Unit) {
                        DropdownMenuItem(text = { Text(label) }, onClick = { menuOpen = false; act() }, modifier = Modifier.testTag(tag))
                    }
                    item(if (shared) tr("Invite someone") else tr("Share…"), "menuShare", listMenu.share)
                    item(tr("Rename"), "menuRename", listMenu.rename)
                    item(tr("Color and icon"), "styleOpen", listMenu.style)
                    item(tr("Currency symbol"), "currencyOpen") { currency = true }
                    env.onExport?.let { ex -> item(tr("Export for a spreadsheet (CSV)"), "export") { ex(list) } }
                    // U-S25: rolling back a partner's changes was built but unreachable.
                    if (shared && people.size > 1) item(tr("Undo someone's changes…"), "menuUndo", listMenu.undo)
                    HorizontalDivider()
                    DropdownMenuItem(
                        text = { Text(tr("Delete budget"), color = MaterialTheme.colorScheme.error) },
                        leadingIcon = { Icon(Ic.Delete, null, tint = MaterialTheme.colorScheme.error) },
                        onClick = { menuOpen = false; listMenu.delete() }, modifier = Modifier.testTag("deleteList"),
                    )
                }
            }
        }
        if (shared && people.size <= 1) {
            // U-S26: shared, but nobody has joined yet.
            Row(Modifier.padding(start = if (onMenu != null) 52.dp else 16.dp, end = 8.dp).testTag("waitingJoin"), verticalAlignment = Alignment.CenterVertically) {
                Text(tr("Invite made. Waiting for someone to join."), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.weight(1f, fill = false))
                TextButton(onClick = listMenu.share) { Text(tr("Show invite")) }
            }
        }
        if (shared && people.size > 1) {
            Text(
                tr("Shared with ") + people.filter { !it.isMe }.joinToString { personLabel(it, people) },
                style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis,
                modifier = Modifier.padding(start = if (onMenu != null) 52.dp else 16.dp, end = 16.dp).testTag("sharedWith"),
            )
            // Whether the others can have it yet (cold UX review U-T4): read again every 15 s while it is open.
            val sync by androidx.compose.runtime.produceState<com.jurisgames.sobres.core.GroupSync?>(null, node, list.groupId, env.version) {
                while (true) {
                    value = runCatching { node.groupSync(list.groupId) }.getOrNull()
                    kotlinx.coroutines.delay(15_000)
                }
            }
            sync?.let { s -> syncLine(s, node.firstRoundDone())?.let { (text, warn) ->
                Text(
                    text, style = MaterialTheme.typography.bodySmall,
                    color = if (warn) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 2, overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.padding(start = if (onMenu != null) 52.dp else 16.dp, end = 16.dp)
                        .semantics { liveRegion = androidx.compose.ui.semantics.LiveRegionMode.Polite }.testTag("syncLine"),
                )
            } }
        }
        val unreadable = remember(node, env.version, list.id) { runCatching { node.budgetUnreadable(list.id) }.getOrDefault(0u) }
        if (unreadable > 0u) Text(
            if (unreadable == 1u) tr("1 entry could not be read (made by a different app) and counts nowhere") else tr("{n} entries could not be read (made by a different app) and count nowhere", "n" to unreadable),
            style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error,
            modifier = Modifier.padding(start = if (onMenu != null) 52.dp else 16.dp, end = 16.dp).testTag("unreadable"),
        )
        chips?.let { Spacer(Modifier.size(4.dp)); it() }
        // a fixed set of sections: they wrap on a small phone instead of scrolling the last ones out of sight (APP-RULES 196)
        androidx.compose.foundation.layout.FlowRow(Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 4.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            for (s in Section.entries) FilterChip(section == s, { section = s }, label = { Text(tr(s.label)) }, modifier = Modifier.testTag(s.tag))
        }
        HorizontalDivider()
        Box(Modifier.weight(1f).fillMaxWidth()) {
            when (section) {
                Section.Envelopes -> EnvelopesView(b, accent, onFill = { fill = 0L }, onOpen = { openEnv = it.id }, onNew = { newEnv = true })
                Section.Activity -> ActivityView(b, activity, repeating, shared, onOpen = { add = AddRequest(it.kind, it) }, onRepeat = { add = AddRequest(it.tx.kind, repeat = it) })
                Section.Accounts -> AccountsView(b, onOpen = { editAcct = it }, onNew = { newAcct = true })
                Section.Month -> MonthView(env, list, b, activity)
            }
        }
        HorizontalDivider()
        Row(Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 8.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(onClick = { add = AddRequest("exp") }, modifier = Modifier.weight(1f).testTag("addSpent")) {
                Icon(Ic.Add, null, Modifier.size(18.dp)); Spacer(Modifier.width(6.dp)); Text(tr("Spent…"))
            }
            OutlinedButton(onClick = { add = AddRequest("inc") }, modifier = Modifier.testTag("addIncome")) { Text(tr("Got money")) }
        }
    }

    // Drawn before the Add sheet: "Add a missing entry" opens it on top, and the list below updates.
    reconcile?.let { id ->
        b.accounts.find { it.id == id }?.let { a ->
            ReconcileDialog(env, list, b, a, activity, onClose = { reconcile = null }, onAdd = { add = AddRequest("exp", account = a.id) }, onDone = { n ->
                reconcile = null
                env.changed()
                scope.launch { snackbar.showSnackbar(tr("Reconciled {name}", "name" to a.name) + if (n > 0) " · " + (if (n == 1) tr("1 entry") else tr("{n} entries", "n" to n.toString())) else "") }
            })
        } ?: run { reconcile = null }
    }
    add?.let { req ->
        AddSheet(env, list, b, activity, req, onClose = { add = null }, onSaved = { msg, undo, fillWith ->
            add = null
            env.changed()
            if (fillWith != null) fill = fillWith
            undoable(msg, undo)
        })
    }
    fill?.let { extra -> FillDialog(env, list, b, extra, onClose = { fill = null }, onSaved = { id -> fill = null; env.changed(); undoable(tr("Filled")) { node.deleteTx(list.id, id) } }) }
    openEnv?.let { id ->
        b.envelopes.find { it.id == id }?.let { e ->
            EnvelopeDialog(b, e, activity, onClose = { openEnv = null },
                onMove = { openEnv = null; add = AddRequest("fill", envelope = e.id) },
                onEdit = { openEnv = null; editEnv = e },
                onDelete = {
                    openEnv = null
                    runCatching { node.deleteEnvelope(list.id, e.id, false) }
                    env.changed()
                    undoable(tr("Deleted {name}", "name" to e.name)) { node.deleteEnvelope(list.id, e.id, true) }
                },
                onEntry = { t -> openEnv = null; add = AddRequest(t.kind, t) })
        } ?: run { openEnv = null }
    }
    if (newEnv || editEnv != null) {
        EnvelopeEditDialog(b, editEnv, onClose = { newEnv = false; editEnv = null }) { name, amount, kind, due, group ->
            runCatching { node.setEnvelope(list.id, editEnv?.id, name, amount, kind, due, group) }
                .onSuccess { newEnv = false; editEnv = null }
                .onFailure { couldNotSave(it) }
            env.changed()
        }
    }
    if (newAcct || editAcct != null) {
        AccountDialog(b, editAcct, onClose = { newAcct = false; editAcct = null },
            onReconcile = editAcct?.let { a -> { editAcct = null; reconcile = a.id } },
            onDelete = editAcct?.let { a ->
                {
                    runCatching { node.deleteAccount(list.id, a.id, false) }
                    editAcct = null
                    env.changed()
                    undoable(tr("Deleted {name}", "name" to a.name)) { node.deleteAccount(list.id, a.id, true) }
                }
            }) { name, kind, start ->
            runCatching { node.setAccount(list.id, editAcct?.id, name, kind, start) }
                .onSuccess { newAcct = false; editAcct = null }
                .onFailure { couldNotSave(it) }
            env.changed()
        }
    }
    if (currency) {
        var sym by remember { mutableStateOf(b.currency) }
        AlertDialog(
            onDismissRequest = { currency = false },
            confirmButton = { TextButton(onClick = { runCatching { node.setCurrency(list.id, sym) }.onSuccess { currency = false }.onFailure { couldNotSave(it) }; env.changed() }, modifier = Modifier.testTag("currencyGo")) { Text(tr("Save")) } },
            dismissButton = { TextButton(onClick = { currency = false }) { Text(tr("Cancel")) } },
            title = { Text(tr("Currency symbol")) },
            text = { OutlinedTextField(sym, { if (it.length <= 8) sym = it }, singleLine = true, placeholder = { Text("$  €  £  ARS") }, modifier = Modifier.testTag("currencyText")) },
        )
    }
}

// ---- Envelopes -------------------------------------------------------------------------------

@Composable
private fun EnvelopesView(b: BudgetView, accent: Color, onFill: () -> Unit, onOpen: (EnvelopeItem) -> Unit, onNew: () -> Unit) {
    val short = b.available < 0
    LazyColumn(Modifier.fillMaxSize()) {
        item {
            Row(Modifier.fillMaxWidth().padding(start = 16.dp, end = 8.dp, top = 8.dp, bottom = 4.dp), verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    // U-S3: "Available" read like "money I have"; this is the money no envelope holds yet.
                    Text(tr("Not in an envelope yet"), style = MaterialTheme.typography.titleMedium)
                    if (short) Text(tr("Envelopes hold more than your accounts"), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
                }
                Text(money(b.available, b.currency), style = MaterialTheme.typography.titleLarge, fontWeight = FontWeight.SemiBold,
                    color = if (short) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface, modifier = Modifier.testTag("available"))
                TextButton(onClick = onFill, modifier = Modifier.testTag("fillOpen")) { Text(tr("Fill")) }
            }
            if (b.envelopes.isNotEmpty() && b.envelopes.all { it.balance == 0L } && b.available > 0) {
                Text(tr("Put your money in envelopes: tap Fill"), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.primary, modifier = Modifier.padding(horizontal = 16.dp).testTag("fillHint"))
            }
            HorizontalDivider(Modifier.padding(top = 4.dp))
        }
        val byGroup = b.envelopes.groupBy { it.group.trim() }
        for ((group, envs) in byGroup) {
            if (group.isNotEmpty()) item(key = "g-$group") {
                Row(Modifier.fillMaxWidth().padding(start = 16.dp, end = 16.dp, top = 14.dp, bottom = 2.dp)) {
                    Text(group.uppercase(Lang.locale), style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.weight(1f))
                    Text(money(envs.fold(0L) { acc, e -> satAdd(acc, e.balance) }, b.currency), style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
            items(envs, key = { "e-" + it.id }) { e -> EnvelopeRow(b, e, accent) { onOpen(e) } }
        }
        item {
            Row(Modifier.fillMaxWidth().clickable(onClick = onNew).padding(horizontal = 16.dp, vertical = 14.dp).testTag("newEnvelope"), verticalAlignment = Alignment.CenterVertically) {
                Icon(Ic.Add, null, Modifier.size(18.dp)); Spacer(Modifier.width(10.dp)); Text(tr("New envelope"))
            }
        }
    }
}

/** What an envelope's bar is measured against, and the line under it. */
private fun target(e: EnvelopeItem, cur: String): Pair<Long, String> = when (e.kind) {
    "y" -> e.amount to tr("of {x} a year", "x" to money(e.amount, cur))
    "g" -> e.amount to (e.due?.let { d -> tr("goal {x} by {date}", "x" to money(e.amount, cur), "date" to (runCatching { shortName(DateTimeFormatter.ofPattern("MMM yyyy", Lang.locale).format(LocalDate.parse(d))) }.getOrDefault(d))) } ?: tr("goal {x}", "x" to money(e.amount, cur)))
    else -> e.amount to tr("of {x} a month", "x" to money(e.amount, cur))
}

@Composable
private fun EnvelopeRow(b: BudgetView, e: EnvelopeItem, accent: Color, onClick: () -> Unit) {
    val neg = e.balance < 0
    val (of, line) = target(e, b.currency)
    Column(Modifier.fillMaxWidth().heightIn(min = 48.dp).clickable(onClick = onClick).padding(horizontal = 16.dp, vertical = 8.dp).testTag("env-" + e.name), verticalArrangement = Arrangement.Center) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(e.name, Modifier.weight(1f), maxLines = 1, overflow = TextOverflow.Ellipsis)
            Text(money(e.balance, b.currency), fontWeight = FontWeight.SemiBold, color = if (neg) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface, modifier = Modifier.testTag("bal-" + e.name))
        }
        if (of > 0) {
            Spacer(Modifier.height(4.dp))
            Bar(if (neg) 1f else (e.balance.toFloat() / of).coerceIn(0f, 1f), if (neg) MaterialTheme.colorScheme.error else accent)
            Text(line, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

@Composable
private fun Bar(fraction: Float, color: Color, modifier: Modifier = Modifier) {
    Box(modifier.fillMaxWidth().height(6.dp).background(MaterialTheme.colorScheme.surfaceVariant, RoundedCornerShape(3.dp))) {
        if (fraction > 0f) Box(Modifier.fillMaxWidth(fraction).fillMaxHeight().background(color, RoundedCornerShape(3.dp)))
    }
}

@Composable
private fun EnvelopeDialog(b: BudgetView, e: EnvelopeItem, activity: List<TxItem>, onClose: () -> Unit, onMove: () -> Unit, onEdit: () -> Unit, onDelete: () -> Unit, onEntry: (TxItem) -> Unit) {
    val mine = remember(activity, e.id) { activity.filter { t -> t.parts.any { it.envelopeId == e.id } }.take(30) }
    AlertDialog(
        onDismissRequest = onClose,
        confirmButton = { TextButton(onClick = onClose, modifier = Modifier.testTag("envDone")) { Text(tr("Done")) } },
        title = { Text(e.name) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                Text(money(e.balance, b.currency), style = MaterialTheme.typography.headlineMedium, fontWeight = FontWeight.SemiBold,
                    color = if (e.balance < 0) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface, modifier = Modifier.testTag("envBalance"))
                if (e.amount > 0) Text(target(e, b.currency).second, color = MaterialTheme.colorScheme.onSurfaceVariant)
                e.pace?.let { p ->
                    Text(
                        if (p >= 0) tr("On track · {x} ahead", "x" to money(p, b.currency)) else tr("{x} behind", "x" to money(-p, b.currency)),
                        color = if (p >= 0) gainColor() else MaterialTheme.colorScheme.error, modifier = Modifier.testTag("envPace"),
                    )
                }
                if (e.kind != "m" && e.monthlyShare > 0) Text(tr("Set aside {x} a month", "x" to money(e.monthlyShare, b.currency)), style = MaterialTheme.typography.bodySmall)
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    OutlinedButton(onClick = onMove, modifier = Modifier.testTag("envMove")) { Text(tr("Move money")) }
                    OutlinedButton(onClick = onEdit, modifier = Modifier.testTag("envEdit")) { Text(tr("Edit")) }
                }
                TextButton(onClick = onDelete, modifier = Modifier.testTag("envDelete")) { Text(tr("Delete envelope"), color = MaterialTheme.colorScheme.error) }
                if (mine.isNotEmpty()) {
                    HorizontalDivider()
                    Column(Modifier.heightIn(max = 220.dp).verticalScroll(rememberScrollState())) {
                        for (t in mine) {
                            val c = t.parts.filter { it.envelopeId == e.id }.sumOf { it.cents }
                            Row(Modifier.fillMaxWidth().clickable { onEntry(t) }.padding(vertical = 6.dp)) {
                                Text(dayLabel(t.date) + " · " + entryTitle(t), Modifier.weight(1f), maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodyMedium)
                                val shown = if (t.kind == "exp") -c else c
                                Text(money(shown, b.currency, sign = shown > 0), color = if (shown > 0) gainColor() else MaterialTheme.colorScheme.onSurface, style = MaterialTheme.typography.bodyMedium)
                            }
                        }
                    }
                }
            }
        },
    )
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun EnvelopeEditDialog(b: BudgetView, e: EnvelopeItem?, onClose: () -> Unit, onSave: (String, Long, String, String?, String) -> Unit) {
    var name by remember { mutableStateOf(e?.name ?: "") }
    var amount by remember { mutableStateOf(e?.let { centsText(it.amount) } ?: "") }
    var kind by remember { mutableStateOf(e?.kind ?: "m") }
    var due by remember { mutableStateOf(e?.due ?: LocalDate.now().plusMonths(6).withDayOfMonth(1).toString()) }
    var group by remember { mutableStateOf(e?.group ?: "") }
    val cents = parseCents(amount)
    val dueOk = kind != "g" || runCatching { LocalDate.parse(due) }.isSuccess
    val ok = name.isNotBlank() && (amount.isBlank() || (cents != null && cents >= 0)) && dueOk
    AlertDialog(
        onDismissRequest = onClose,
        confirmButton = { TextButton(onClick = { if (ok) onSave(name.trim(), cents ?: 0, kind, if (kind == "g") due else null, group.trim()) }, enabled = ok, modifier = Modifier.testTag("envSave")) { Text(tr("Save")) } },
        dismissButton = { TextButton(onClick = onClose) { Text(tr("Cancel")) } },
        title = { Text(if (e == null) tr("New envelope") else tr("Edit envelope")) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedTextField(name, { name = it }, singleLine = true, label = { Text(tr("Name")) }, modifier = Modifier.testTag("envName"))
                FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    for ((k, l) in listOf("m" to "Monthly", "y" to "Yearly", "g" to "Goal")) FilterChip(kind == k, { kind = k }, label = { Text(tr(l)) }, modifier = Modifier.testTag("envKind-$k"))
                }
                OutlinedTextField(amount, { amount = it }, singleLine = true, keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal),
                    label = { Text(when (kind) { "y" -> tr("Amount a year"); "g" -> tr("Goal amount"); else -> tr("Amount a month") }) },
                    prefix = if (b.currency.isNotEmpty()) ({ Text(b.currency) }) else null, modifier = Modifier.testTag("envAmount"))
                // U-S6: the goal's date from a calendar; shown as month and year ("By May 2027").
                var pickDue by remember { mutableStateOf(false) }
                if (pickDue) PickDateDialog(due, onPicked = { due = it }, onClose = { pickDue = false })
                if (kind == "g") OutlinedButton(onClick = { pickDue = true }, modifier = Modifier.heightIn(min = 48.dp).testTag("envDue")) {
                    Text(tr("By {date}", "date" to (runCatching { shortName(DateTimeFormatter.ofPattern("MMMM yyyy", Lang.locale).format(LocalDate.parse(due))) }.getOrDefault(due))))
                }
                OutlinedTextField(group, { group = it }, singleLine = true, label = { Text(tr("Heading (optional)")) }, modifier = Modifier.testTag("envGroup"))
                val headings = b.envelopes.map { it.group }.filter { it.isNotBlank() }.distinct()
                if (headings.isNotEmpty()) FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    for (h in headings) FilterChip(group == h, { group = h }, label = { Text(h) })
                }
            }
        },
    )
}

// ---- Fill ------------------------------------------------------------------------------------

@Composable
private fun FillDialog(env: Env, list: ListItem, b: BudgetView, extra: Long, onClose: () -> Unit, onSaved: (String) -> Unit) {
    val node = env.node
    val fields = remember { mutableStateOf(b.envelopes.associate { it.id to "" }) }
    fun applyPlan(rule: String) {
        val plan = runCatching { node.fillPlan(list.id, rule, today()) }.getOrDefault(emptyList()).associate { it.envelopeId to it.cents }
        fields.value = b.envelopes.associate { it.id to centsText(plan[it.id] ?: 0) }
    }
    val parts = fields.value.mapNotNull { (id, t) -> parseCents(t)?.takeIf { it != 0L }?.let { id to it } }
    val bad = fields.value.values.any { it.isNotBlank() && parseCents(it) == null }
    val left = b.available - parts.sumOf { it.second }
    AlertDialog(
        onDismissRequest = onClose,
        confirmButton = {
            TextButton(enabled = parts.isNotEmpty() && !bad, onClick = {
                val total = parts.sumOf { it.second }
                runCatching {
                    node.setTx(list.id, null, TxEdit("fill", today(), total, "", "", "", "", parts.map { PartItem(it.first, "", it.second) }))
                }.onSuccess(onSaved)
            }, modifier = Modifier.testTag("fillSave")) { Text(tr("Fill")) }
        },
        dismissButton = { TextButton(onClick = onClose) { Text(tr("Cancel")) } },
        title = { Text(tr("Fill envelopes")) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(tr("Not in an envelope yet"), Modifier.weight(1f))
                    Text(money(b.available, b.currency), fontWeight = FontWeight.SemiBold)
                }
                if (extra > 0) Text(tr("Including the {x} you just got", "x" to money(extra, b.currency)), style = MaterialTheme.typography.bodySmall)
                FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    OutlinedButton(onClick = { applyPlan("add") }, modifier = Modifier.testTag("fillMonthly")) { Text(tr("Add each month's amount")) }
                    OutlinedButton(onClick = { applyPlan("top") }, modifier = Modifier.testTag("fillTop")) { Text(tr("Fill up to each month's amount")) }
                }
                Column(Modifier.heightIn(max = 300.dp).verticalScroll(rememberScrollState())) {
                    for (e in b.envelopes) {
                        Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.padding(vertical = 2.dp)) {
                            Column(Modifier.weight(1f)) {
                                Text(e.name, maxLines = 1, overflow = TextOverflow.Ellipsis)
                                Text(tr("has {x}", "x" to money(e.balance, b.currency)), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                            }
                            val t = fields.value[e.id].orEmpty()
                            OutlinedTextField(t, { v -> fields.value = fields.value + (e.id to v) }, singleLine = true, isError = t.isNotBlank() && parseCents(t) == null,
                                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal), placeholder = { Text(zeroAmount()) },
                                modifier = Modifier.weight(1f).widthIn(min = 96.dp).testTag("fill-" + e.name))
                        }
                    }
                }
                Row {
                    Text(tr("Left over"), Modifier.weight(1f))
                    Text(money(left, b.currency), fontWeight = FontWeight.SemiBold, color = if (left < 0) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface, modifier = Modifier.testTag("fillLeft"))
                }
                // U-S17: allowed, but said.
                if (left < 0) Text(tr("That's {x} more than you have.", "x" to money(-left, b.currency)), style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error, modifier = Modifier.testTag("fillOver"))
            }
        },
    )
}

// ---- Add / edit an entry ---------------------------------------------------------------------

private val KINDS = listOf("exp" to "Spent", "inc" to "Got money", "fill" to "Move between envelopes", "xfer" to "Move between accounts")

/** "" = Available, as the "from" of a move. */
private const val AVAILABLE = ""

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun AddSheet(
    env: Env,
    list: ListItem,
    b: BudgetView,
    activity: List<TxItem>,
    req: AddRequest,
    onClose: () -> Unit,
    onSaved: (message: String, undo: () -> Unit, fillWith: Long?) -> Unit,
) {
    val node = env.node
    val edit = req.edit
    val rep = req.repeat
    val src = edit ?: rep?.tx
    val payees = remember(node, env.version, list.id) { runCatching { node.payees(list.id) }.getOrDefault(emptyList()) }
    val lastAccount = activity.firstOrNull { it.kind == "exp" }?.accountId ?: b.accounts.firstOrNull()?.id ?: ""
    // U-S5: what is typed survives rotation and Android recreating the app.
    var kind by rememberSaveable { mutableStateOf(src?.kind ?: req.kind) }
    var refund by rememberSaveable { mutableStateOf(src != null && src.kind == "exp" && src.amount < 0) }
    var amount by rememberSaveable { mutableStateOf(src?.let { centsText(kotlin.math.abs(if (it.kind == "fill") it.parts.filter { p -> p.cents > 0 }.sumOf { p -> p.cents } else it.amount)) } ?: "") }
    var payee by rememberSaveable { mutableStateOf(src?.payee ?: "") }
    var note by rememberSaveable { mutableStateOf(src?.note ?: "") }
    var date by rememberSaveable { mutableStateOf(src?.date ?: today()) }
    var account by rememberSaveable { mutableStateOf(src?.accountId?.takeIf { it.isNotEmpty() } ?: req.account ?: lastAccount) }
    var toAccount by rememberSaveable { mutableStateOf(src?.toId?.takeIf { it.isNotEmpty() } ?: b.accounts.firstOrNull { it.id != account }?.id ?: "") }
    // Spent: one envelope, or a split (envelope -> typed amount)
    // U-S4: a new expense starts in the envelope used last, so Save works after amount and "what for".
    val lastEnvelope = activity.firstOrNull { it.kind == "exp" && it.parts.size == 1 }?.parts?.single()?.envelopeId?.takeIf { id -> b.envelopes.any { it.id == id } }
    var envelope by rememberSaveable { mutableStateOf(src?.parts?.singleOrNull()?.envelopeId ?: req.envelope ?: lastEnvelope ?: b.envelopes.singleOrNull()?.id ?: "") }
    var split by rememberSaveable { mutableStateOf(src != null && src.kind == "exp" && src.parts.size > 1) }
    var splitText by remember { mutableStateOf(src?.takeIf { it.kind == "exp" }?.parts?.associate { it.envelopeId to centsText(kotlin.math.abs(it.cents)) } ?: emptyMap()) }
    // Move: from (envelope or Available) -> to
    var from by rememberSaveable { mutableStateOf(src?.takeIf { it.kind == "fill" }?.parts?.firstOrNull { it.cents < 0 }?.envelopeId ?: req.envelope ?: AVAILABLE) }
    var to by rememberSaveable { mutableStateOf(src?.takeIf { it.kind == "fill" }?.parts?.firstOrNull { it.cents > 0 }?.envelopeId ?: b.envelopes.firstOrNull { it.id != from }?.id ?: "") }
    var fillAfter by rememberSaveable { mutableStateOf(false) }
    var every by rememberSaveable { mutableStateOf(rep?.every ?: "") }
    var pickDate by rememberSaveable { mutableStateOf(false) }
    var tried by rememberSaveable { mutableStateOf(false) }
    var saveError by remember { mutableStateOf<String?>(null) }
    val focus = remember { FocusRequester() }
    androidx.compose.runtime.LaunchedEffect(Unit) { kotlinx.coroutines.delay(100); runCatching { focus.requestFocus() } }

    // A new repeat starts at most a year back (round 5, S2; the core refuses it too).
    val tooOld = rep == null && edit == null && every.isNotEmpty() &&
        runCatching { LocalDate.parse(date) < LocalDate.parse(today()).minusDays(366) }.getOrDefault(false)
    val cents = parseCents(amount)?.takeIf { it > 0 }
    val splitParts = splitText.mapNotNull { (id, t) -> parseCents(t)?.takeIf { it > 0 }?.let { id to it } }
    val splitLeft = satAdd(cents ?: 0, -splitParts.fold(0L) { acc, p -> satAdd(acc, p.second) })
    val sign = if (refund) -1 else 1
    val input: TxEdit? = cents?.let { c ->
        when (kind) {
            "exp" -> {
                val parts = if (split) splitParts.map { PartItem(it.first, "", sign * it.second) } else listOf(PartItem(envelope, "", sign * c))
                if (account.isEmpty() || (split && splitLeft != 0L) || (!split && envelope.isEmpty())) null
                else TxEdit("exp", date, sign * c, account, "", payee.trim(), note.trim(), parts)
            }
            "inc" -> if (account.isEmpty()) null else TxEdit("inc", date, c, account, "", payee.trim(), note.trim(), emptyList())
            "fill" -> if (to.isEmpty() || to == from) null else TxEdit("fill", date, c, "", "", "", note.trim(),
                listOfNotNull(PartItem(to, "", c), if (from != AVAILABLE) PartItem(from, "", -c) else null))
            "xfer" -> if (account.isEmpty() || toAccount.isEmpty() || account == toAccount) null else TxEdit("xfer", date, c, account, toAccount, "", note.trim(), emptyList())
            else -> null
        }
    }
    // U-S4: why Save can't go yet, said under the amount once Save is tapped.
    val missing: String? = when {
        cents == null -> if (amount.isBlank()) tr("Type an amount") else tr("Use numbers, like {x}", "x" to money(1250L))
        kind == "exp" && b.accounts.isEmpty() || kind == "inc" && b.accounts.isEmpty() -> tr("Add an account first (Accounts)")
        (kind == "exp" || kind == "inc" || kind == "xfer") && account.isEmpty() -> tr("Pick an account")
        kind == "exp" && !split && b.envelopes.isEmpty() -> tr("Add an envelope first")
        kind == "exp" && !split && envelope.isEmpty() -> tr("Pick an envelope")
        kind == "exp" && split && splitLeft != 0L -> tr("{x} still to split", "x" to money(splitLeft, b.currency))
        kind == "fill" && (to.isEmpty() || to == from) -> tr("Pick where the money goes")
        kind == "xfer" && (toAccount.isEmpty() || account == toAccount) -> tr("Pick two different accounts")
        else -> null
    }
    fun save() {
        if (input == null) { tried = true; return }
        val t = input
        if (every.isNotEmpty() && edit == null) {
            val id = runCatching { node.setRepeating(list.id, rep?.id, every, t) }.onFailure { saveError = userError(it, tr("Couldn't save. Try again.")) }.getOrNull() ?: return
            val posted = runCatching { node.postDue(list.id, today()) }.getOrDefault(emptyList())
            val undo: () -> Unit = if (rep == null) ({
                node.deleteRepeating(list.id, id, false)
                for (p in posted) node.deleteTx(list.id, p)
            }) else ({
                node.setRepeating(list.id, rep.id, rep.every, rep.tx.toEdit())
                for (p in posted) node.deleteTx(list.id, p)
            })
            onSaved(if (rep == null) tr("Saved") else tr("Changed"), undo, null)
            return
        }
        val id = runCatching { node.setTx(list.id, edit?.id, t) }.onFailure { saveError = userError(it, tr("Couldn't save. Try again.")) }.getOrNull() ?: return
        val undo: () -> Unit = if (edit == null) ({ node.deleteTx(list.id, id) }) else ({ node.restoreTx(list.id, edit) })
        onSaved(if (edit == null) tr("Saved") else tr("Changed"), undo, if (kind == "inc" && fillAfter && edit == null) t.amount else null)
    }

    val typedSomething = amount.isNotBlank() || payee.isNotBlank() || note.isNotBlank()
    val enter = KeyboardActions(onDone = { save() })
    // U-S6: dates come from a calendar in the person's own order.
    if (pickDate) PickDateDialog(date, onPicked = { date = it }, onClose = { pickDate = false })
    AlertDialog(
        onDismissRequest = onClose,
        // U-S5: a stray tap beside the sheet doesn't throw away what was typed (Back and Cancel still close it).
        properties = DialogProperties(dismissOnClickOutside = !typedSomething),
        // U-S4: Save stays tappable; when something is missing it says what, under the amount.
        confirmButton = { TextButton(onClick = ::save, enabled = !tooOld, modifier = Modifier.testTag("addSave")) { Text(tr("Save")) } },
        // U-S15: only Cancel and Save in the footer; the red actions live at the bottom of the sheet.
        dismissButton = { TextButton(onClick = onClose, modifier = Modifier.testTag("addCancel")) { Text(tr("Cancel")) } },
        text = {
            Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    for ((k, l) in KINDS) FilterChip(kind == k, { kind = k }, label = { Text(tr(l)) }, modifier = Modifier.testTag("kind-$k"))
                }
                OutlinedTextField(amount, { amount = it }, singleLine = true, keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal, imeAction = ImeAction.Done), keyboardActions = enter,
                    label = { Text(tr("Amount")) }, isError = amount.isNotBlank() && cents == null,
                    supportingText = if (amount.isNotBlank() && cents == null) ({ Text(tr("Use numbers, like {x}", "x" to money(1250L))) }) else null,
                    prefix = if (b.currency.isNotEmpty()) ({ Text(b.currency) }) else null,
                    modifier = Modifier.fillMaxWidth().focusRequester(focus).testTag("amount"))
                if (tried && missing != null) Text(missing, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodyMedium,
                    modifier = Modifier.semantics { liveRegion = androidx.compose.ui.semantics.LiveRegionMode.Polite }.testTag("addMissing"))
                saveError?.let { Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.testTag("addError")) }
                when (kind) {
                    "exp" -> {
                        OutlinedTextField(payee, { payee = it }, singleLine = true, label = { Text(tr("What for")) },
                            keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done), keyboardActions = enter, modifier = Modifier.fillMaxWidth().testTag("payee"))
                        val suggest = payees.filter { it.payee.isNotBlank() && it.payee != payee && (payee.isBlank() || it.payee.contains(payee.trim(), ignoreCase = true)) }.take(4)
                        if (suggest.isNotEmpty() && edit == null) FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                            for (p in suggest) FilterChip(false, {
                                payee = p.payee
                                p.envelopeId?.takeIf { id -> b.envelopes.any { it.id == id } }?.let { envelope = it; split = false }
                                if (b.accounts.any { it.id == p.accountId }) account = p.accountId
                            }, label = { Text(p.payee) }, modifier = Modifier.testTag("payee-" + p.payee))
                        }
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Text(tr("Envelope"), style = MaterialTheme.typography.labelLarge, modifier = Modifier.weight(1f))
                            TextButton(onClick = { split = !split }, modifier = Modifier.testTag("split")) { Text(if (split) tr("One envelope") else tr("Split")) }
                        }
                        if (!split) {
                            FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                                for (e in b.envelopes) FilterChip(envelope == e.id, { envelope = e.id }, label = { Text(tr("{name} · {x} left", "name" to e.name, "x" to money(e.balance, b.currency))) }, modifier = Modifier.testTag("envChip-" + e.name))
                            }
                            // U-S3: say it before the envelope goes below zero.
                            val picked = b.envelopes.find { it.id == envelope }
                            if (picked != null && cents != null && !refund && edit == null && picked.balance - cents < 0) Text(
                                tr("{name} will be {x} short. Fill it from Not in an envelope yet.", "name" to picked.name, "x" to money(cents - picked.balance, b.currency)),
                                style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error, modifier = Modifier.testTag("envShort"))
                        } else {
                            for (e in b.envelopes) Row(verticalAlignment = Alignment.CenterVertically) {
                                Text(e.name, Modifier.weight(1f))
                                OutlinedTextField(splitText[e.id].orEmpty(), { splitText = splitText + (e.id to it) }, singleLine = true, placeholder = { Text(zeroAmount()) },
                                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal), modifier = Modifier.weight(1f).widthIn(min = 96.dp).testTag("splitAmt-" + e.name))
                            }
                            Text(if (splitLeft == 0L) tr("Adds up") else tr("{x} still to split", "x" to money(splitLeft, b.currency)),
                                color = if (splitLeft == 0L) gainColor() else MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall, modifier = Modifier.testTag("splitLeft"))
                        }
                        AccountChips(b, tr("Paid from"), account, "acct") { account = it }
                        Row(Modifier.fillMaxWidth().heightIn(min = 48.dp).toggleable(refund, role = Role.Switch, onValueChange = { refund = it }).testTag("refund"), verticalAlignment = Alignment.CenterVertically) {
                            Text(tr("Refund (money back)"), Modifier.weight(1f))
                            Switch(refund, null)
                        }
                    }
                    "inc" -> {
                        OutlinedTextField(payee, { payee = it }, singleLine = true, label = { Text(tr("From")) },
                            keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done), keyboardActions = enter, modifier = Modifier.fillMaxWidth().testTag("payee"))
                        AccountChips(b, tr("Into"), account, "acct") { account = it }
                        if (edit == null && every.isEmpty()) Row(Modifier.fillMaxWidth().heightIn(min = 48.dp).toggleable(fillAfter, role = Role.Switch, onValueChange = { fillAfter = it }).testTag("fillAfter"), verticalAlignment = Alignment.CenterVertically) {
                            Text(tr("Put it in envelopes now"), Modifier.weight(1f))
                            Switch(fillAfter, null)
                        }
                    }
                    "fill" -> {
                        Text(tr("From"), style = MaterialTheme.typography.labelLarge)
                        FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                            FilterChip(from == AVAILABLE, { from = AVAILABLE }, label = { Text(tr("Not in an envelope yet") + " · " + money(b.available, b.currency)) }, modifier = Modifier.testTag("from-available"))
                            for (e in b.envelopes) FilterChip(from == e.id, { from = e.id }, label = { Text(e.name + " · " + money(e.balance, b.currency)) }, modifier = Modifier.testTag("from-" + e.name))
                        }
                        Text(tr("To"), style = MaterialTheme.typography.labelLarge)
                        FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                            for (e in b.envelopes.filter { it.id != from }) FilterChip(to == e.id, { to = e.id }, label = { Text(e.name) }, modifier = Modifier.testTag("to-" + e.name))
                        }
                    }
                    "xfer" -> {
                        AccountChips(b, tr("From"), account, "acct") { account = it }
                        AccountChips(b, tr("To"), toAccount, "toAcct") { toAccount = it }
                    }
                }
                FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.Center) {
                    val t = today()
                    val y = LocalDate.now().minusDays(1).toString()
                    FilterChip(date == t, { date = t }, label = { Text(tr("Today")) }, modifier = Modifier.testTag("dateToday"))
                    FilterChip(date == y, { date = y }, label = { Text(tr("Yesterday")) }, modifier = Modifier.testTag("dateYesterday"))
                    FilterChip(date != t && date != y, { pickDate = true }, label = { Text(if (date != t && date != y) dayLabel(date) else tr("Pick…")) }, modifier = Modifier.testTag("datePick"))
                }
                if (edit == null) FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    if (rep == null) FilterChip(every.isEmpty(), { every = "" }, label = { Text(tr("Once")) }, modifier = Modifier.testTag("every-once"))
                    for ((k, l) in EVERY) FilterChip(every == k, { every = k }, label = { Text(tr(l)) }, modifier = Modifier.testTag("every-$k"))
                }
                if (rep != null) Text(tr("Changes apply from {date} on", "date" to dayLabel(rep.next)), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                if (tooOld) Text(tr("A repeat can start at most a year back"), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error, modifier = Modifier.testTag("repTooOld"))
                OutlinedTextField(note, { note = it }, singleLine = true, label = { Text(tr("Note")) },
                    keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done), keyboardActions = enter, modifier = Modifier.fillMaxWidth().testTag("note"))
                if (edit != null || rep != null) FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.padding(top = 4.dp)) {
                    if (edit != null) TextButton(onClick = {
                        runCatching { node.deleteTx(list.id, edit.id) }
                        onSaved(tr("Deleted"), { node.restoreTx(list.id, edit) }, null)
                    }, modifier = Modifier.testTag("addDelete")) { Text(tr("Delete"), color = MaterialTheme.colorScheme.error) }
                    if (rep != null) TextButton(onClick = {
                        runCatching { node.deleteRepeating(list.id, rep.id, false) }
                        onSaved(tr("Stopped repeating"), { node.deleteRepeating(list.id, rep.id, true) }, null)
                    }, modifier = Modifier.testTag("repStop")) { Text(tr("Stop repeating"), color = MaterialTheme.colorScheme.error) }
                    // Everything it posted, in one go (round 5, S2), with Undo.
                    if (rep != null) TextButton(onClick = {
                        val gone = runCatching { node.deleteRepeatingAndEntries(list.id, rep.id) }.getOrNull() ?: return@TextButton
                        onSaved(tr("Removed {n} entries", "n" to gone.size), {
                            for (t in gone) node.restoreTx(list.id, t)
                            node.deleteRepeating(list.id, rep.id, true)
                        }, null)
                    }, modifier = Modifier.testTag("repRemoveAll")) { Text(tr("Remove all"), color = MaterialTheme.colorScheme.error) }
                }
            }
        },
    )
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun AccountChips(b: BudgetView, label: String, selected: String, tag: String, onPick: (String) -> Unit) {
    Text(label, style = MaterialTheme.typography.labelLarge)
    FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        for (a in b.accounts) FilterChip(selected == a.id, { onPick(a.id) }, label = { Text(a.name) }, modifier = Modifier.testTag("$tag-" + a.name))
    }
}

/** A line's title: what it was for, else what kind it is. */
private fun entryTitle(t: TxItem): String = when (t.kind) {
    "xfer" -> tr("{a} to {b}", "a" to t.accountName, "b" to t.toName)
    "fill" -> if (t.parts.any { it.cents < 0 }) tr("Moved") + ": " + t.parts.joinToString(" → ") { it.envelopeName } else tr("Filled")
    "inc" -> t.payee.ifBlank { tr("Got money") }
    else -> t.payee.ifBlank { t.parts.joinToString(", ") { it.envelopeName } }
}

// ---- Activity --------------------------------------------------------------------------------

@Composable
private fun ActivityView(b: BudgetView, activity: List<TxItem>, repeating: List<RepeatItem>, shared: Boolean, onOpen: (TxItem) -> Unit, onRepeat: (RepeatItem) -> Unit) {
    var query by remember { mutableStateOf("") }
    val q = query.trim()
    val shown = if (q.isEmpty()) activity else activity.filter { t ->
        t.payee.contains(q, true) || t.note.contains(q, true) || t.parts.any { it.envelopeName.contains(q, true) } || t.accountName.contains(q, true) || t.by.contains(q, true)
    }
    Column(Modifier.fillMaxSize()) {
        OutlinedTextField(query, { query = it }, singleLine = true, placeholder = { Text(tr("Search")) }, leadingIcon = { Icon(Ic.Search, null) },
            modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 6.dp).testTag("activitySearch"))
        if (activity.isEmpty() && repeating.isEmpty()) {
            Text(tr("Nothing yet. What you spend and get shows up here."), color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(24.dp).testTag("activityEmpty"))
        } else if (q.isNotEmpty() && shown.isEmpty()) {
            // U-S21: a search that finds nothing says so.
            Text(tr("Nothing matches “{q}”", "q" to q), color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(24.dp).testTag("searchEmpty"))
        }
        LazyColumn(Modifier.weight(1f)) {
            if (q.isEmpty() && repeating.isNotEmpty()) {
                // Every key carries its type (round 5, S6): an entry id chosen by another member can't collide.
                item(key = "h-repeating") {
                    Text(tr("Repeating"), style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(start = 16.dp, top = 12.dp, bottom = 2.dp))
                }
                items(repeating, key = { "r-" + it.id }) { r ->
                    Row(Modifier.fillMaxWidth().clickable { onRepeat(r) }.padding(horizontal = 16.dp, vertical = 8.dp).testTag("rep-" + entryTitle(r.tx)), verticalAlignment = Alignment.CenterVertically) {
                        Icon(Ic.Repeat, null, Modifier.size(18.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
                        Spacer(Modifier.width(12.dp))
                        Column(Modifier.weight(1f)) {
                            Text(entryTitle(r.tx), maxLines = 1, overflow = TextOverflow.Ellipsis)
                            Text(tr(EVERY.firstOrNull { it.first == r.every }?.second ?: "") + " · " + tr("next {date}", "date" to dayLabel(r.next)),
                                style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.testTag("repNext"))
                        }
                        Text(money(if (r.tx.kind == "exp") -r.tx.amount else r.tx.amount, b.currency, sign = r.tx.kind == "inc"),
                            color = if (r.tx.kind == "inc") gainColor() else MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                }
            }
            for ((d, day) in shown.groupBy { it.date }) {
                item(key = "h-d-$d") {
                    Text(dayLabel(d), style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(start = 16.dp, top = 12.dp, bottom = 2.dp))
                }
                items(day, key = { "t-" + it.id }) { t ->
                    Row(Modifier.fillMaxWidth().clickable { onOpen(t) }.padding(horizontal = 16.dp, vertical = 8.dp).testTag("tx-" + entryTitle(t)), verticalAlignment = Alignment.CenterVertically) {
                        Column(Modifier.weight(1f)) {
                            Text(entryTitle(t), maxLines = 1, overflow = TextOverflow.Ellipsis)
                            val sub = listOfNotNull(
                                t.parts.takeIf { t.kind == "exp" }?.joinToString(", ") { it.envelopeName },
                                t.accountName.takeIf { it.isNotEmpty() && t.kind != "xfer" },
                                t.by.takeIf { shared && it.isNotEmpty() },
                                t.editedBy.takeIf { shared && it.isNotEmpty() }?.let { tr("edited by {name}", "name" to it) },
                                t.note.takeIf { it.isNotEmpty() },
                            ).joinToString(" · ")
                            if (sub.isNotEmpty()) Text(sub, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
                        }
                        val (amt, color) = when (t.kind) {
                            "exp" -> -t.amount to (if (t.amount < 0) gainColor() else MaterialTheme.colorScheme.onSurface)
                            "inc" -> t.amount to gainColor()
                            else -> t.amount to MaterialTheme.colorScheme.onSurfaceVariant
                        }
                        Text(money(amt, b.currency, sign = t.kind == "inc" || (t.kind == "exp" && t.amount < 0)), color = color, fontWeight = FontWeight.Medium)
                        StatusMark(t.status)
                    }
                }
            }
        }
    }
}

/** Cleared (seen at the bank): a check; reconciled (part of a finished statement): a check in a circle. */
@Composable
private fun StatusMark(status: Long) {
    if (status == 0L) return
    Spacer(Modifier.width(6.dp))
    val (icon, label) = if (status >= 2L) Ic.CheckCircle to tr("Reconciled") else Ic.Check to tr("Cleared")
    Icon(icon, label, Modifier.size(16.dp).testTag("status-$status"), tint = MaterialTheme.colorScheme.onSurfaceVariant)
}

// ---- Reconcile -------------------------------------------------------------------------------

/**
 * Goodbudget's reconcile: the statement's date and ending balance, then tick each entry the bank
 * shows. Ticks are saved as they are made (closing keeps them for later); Finish is possible once
 * the cleared balance equals the statement, and turns the ticked entries into reconciled ones.
 */
@Composable
private fun ReconcileDialog(env: Env, list: ListItem, b: BudgetView, a: AccountItem, activity: List<TxItem>, onClose: () -> Unit, onAdd: () -> Unit, onDone: (Int) -> Unit) {
    var date by remember(a.id) { mutableStateOf(today()) }
    var statement by remember(a.id) { mutableStateOf("") }
    val dateOk = runCatching { LocalDate.parse(date.trim()) }.isSuccess
    val stmt = if (statement.isBlank()) null else parseCents(statement)
    fun effect(t: TxItem): Long = when {
        t.kind == "exp" && t.accountId == a.id -> -t.amount
        t.kind == "inc" && t.accountId == a.id -> t.amount
        t.kind == "xfer" && t.accountId == a.id -> -t.amount
        t.kind == "xfer" && t.toId == a.id -> t.amount
        else -> 0L
    }
    // Oldest first, like a statement; only what is not reconciled yet, up to the statement's date.
    val open = activity.filter { it.status < 2L && effect(it) != 0L && (!dateOk || it.date <= date.trim()) }.sortedWith(compareBy({ it.date }, { it.atMs }))
    val diff = stmt?.let { it - a.cleared }
    AlertDialog(
        onDismissRequest = onClose,
        confirmButton = {
            TextButton(enabled = dateOk && diff == 0L, onClick = {
                runCatching { env.node.finishReconcile(list.id, a.id, date.trim(), stmt!!) }.onSuccess { onDone(it.toInt()) }
            }, modifier = Modifier.testTag("recFinish")) { Text(tr("Finish")) }
        },
        dismissButton = { TextButton(onClick = onClose, modifier = Modifier.testTag("recLater")) { Text(tr("Later")) } },
        title = { Text(tr("Reconcile {name}", "name" to a.name)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                if (a.recDate.isNotEmpty()) {
                    Text(tr("Last statement: {date}, {amount}", "date" to dayLabel(a.recDate), "amount" to money(a.recBalance, b.currency)),
                        style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.testTag("recLast"))
                }
                // U-S20: what this is for, in plain words.
                Text(tr("Check your entries against your bank statement."), style = MaterialTheme.typography.bodyMedium)
                var pickStmt by remember { mutableStateOf(false) }
                if (pickStmt) PickDateDialog(date, onPicked = { date = it }, onClose = { pickStmt = false })
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                    // U-S6: the statement's date from a calendar, not typed as year-month-day.
                    OutlinedButton(onClick = { pickStmt = true }, modifier = Modifier.weight(1f).heightIn(min = 48.dp).testTag("recDate")) {
                        Text(tr("Statement of {date}", "date" to (if (dateOk) dayLabel(date.trim()) else "…")), maxLines = 2)
                    }
                    OutlinedTextField(statement, { statement = it }, singleLine = true, isError = statement.isNotBlank() && stmt == null,
                        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal), label = { Text(tr("Balance on the statement")) },
                        prefix = if (b.currency.isNotEmpty()) ({ Text(b.currency) }) else null, modifier = Modifier.weight(1f).testTag("recStatement"))
                }
                Text(tr("Tick each entry your statement shows."), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                Column(Modifier.heightIn(max = 280.dp).verticalScroll(rememberScrollState())) {
                    if (open.isEmpty()) Text(tr("No entries to tick."), color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(vertical = 8.dp))
                    for (t in open) {
                        val ticked = t.status == 1L
                        // U-S20: one control per row for screen readers, the checkbox only shows the state.
                        Row(Modifier.fillMaxWidth().heightIn(min = 48.dp).toggleable(ticked, role = Role.Checkbox, onValueChange = { v -> runCatching { env.node.setCleared(list.id, t.id, v) }; env.changed() }).testTag("rec-" + entryTitle(t)),
                            verticalAlignment = Alignment.CenterVertically) {
                            Checkbox(ticked, null)
                            Column(Modifier.weight(1f)) {
                                Text(entryTitle(t), maxLines = 1, overflow = TextOverflow.Ellipsis)
                                Text(dayLabel(t.date), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                            }
                            Text(money(effect(t), b.currency, sign = effect(t) > 0), color = if (effect(t) > 0) gainColor() else MaterialTheme.colorScheme.onSurface)
                        }
                    }
                }
                TextButton(onClick = onAdd, modifier = Modifier.testTag("recAdd")) {
                    Icon(Ic.Add, null, Modifier.size(18.dp)); Spacer(Modifier.width(6.dp)); Text(tr("Add a missing entry"))
                }
                HorizontalDivider()
                Row { Text(tr("Ticked so far"), Modifier.weight(1f)); Text(money(a.cleared, b.currency), fontWeight = FontWeight.SemiBold, modifier = Modifier.testTag("recCleared")) }
                if (diff != null) {
                    Row {
                        Text(tr("Difference"), Modifier.weight(1f))
                        Text(if (diff == 0L) tr("Matches the statement") else money(diff, b.currency, sign = true),
                            color = if (diff == 0L) gainColor() else MaterialTheme.colorScheme.error, fontWeight = FontWeight.SemiBold, modifier = Modifier.testTag("recDiff"))
                    }
                }
            }
        },
    )
}

// ---- Accounts --------------------------------------------------------------------------------

private val ACCOUNT_KINDS = listOf("bank" to "Bank", "cash" to "Cash", "card" to "Card")

@Composable
private fun AccountsView(b: BudgetView, onOpen: (AccountItem) -> Unit, onNew: () -> Unit) {
    LazyColumn(Modifier.fillMaxSize()) {
        items(b.accounts, key = { "a-" + it.id }) { a ->
            Row(Modifier.fillMaxWidth().clickable { onOpen(a) }.padding(horizontal = 16.dp, vertical = 12.dp).testTag("acct-" + a.name), verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text(a.name)
                    val kindName = tr(ACCOUNT_KINDS.find { it.first == a.kind }?.second ?: "Bank")
                    Text(if (a.recDate.isEmpty()) kindName else kindName + " · " + tr("reconciled {date}", "date" to dayLabel(a.recDate)),
                        style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
                Text(money(a.balance, b.currency), fontWeight = FontWeight.SemiBold, color = if (a.balance < 0) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface)
            }
        }
        item {
            HorizontalDivider()
            Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp)) {
                Text(tr("Total"), Modifier.weight(1f), fontWeight = FontWeight.SemiBold)
                Text(money(b.accounts.sumOf { it.balance }, b.currency), fontWeight = FontWeight.SemiBold, modifier = Modifier.testTag("acctTotal"))
            }
            Row(Modifier.fillMaxWidth().clickable(onClick = onNew).padding(horizontal = 16.dp, vertical = 14.dp).testTag("newAccount"), verticalAlignment = Alignment.CenterVertically) {
                Icon(Ic.Add, null, Modifier.size(18.dp)); Spacer(Modifier.width(10.dp)); Text(tr("New account"))
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun AccountDialog(b: BudgetView, a: AccountItem?, onClose: () -> Unit, onReconcile: (() -> Unit)?, onDelete: (() -> Unit)?, onSave: (String, String, Long) -> Unit) {
    var name by remember { mutableStateOf(a?.name ?: "") }
    var kind by remember { mutableStateOf(a?.kind ?: "bank") }
    // U-S13: no typed minus sign (number keyboards often lack one): a card asks what is owed, others have "Overdrawn".
    var balance by remember { mutableStateOf(a?.let { centsText(if (it.kind == "card") -it.balance else kotlin.math.abs(it.balance)) } ?: "") }
    var overdrawn by remember { mutableStateOf(a != null && a.kind != "card" && a.balance < 0) }
    val typed = if (balance.isBlank()) 0L else parseCents(balance)
    val cents = typed?.let { if (kind == "card" || overdrawn) -it else it }
    val ok = name.isNotBlank() && cents != null
    AlertDialog(
        onDismissRequest = onClose,
        confirmButton = {
            TextButton(enabled = ok, onClick = {
                // What is in it today = the opening balance plus everything since, so the opening balance moves.
                val start = if (a == null) cents!! else a.start + (cents!! - a.balance)
                onSave(name.trim(), kind, start)
            }, modifier = Modifier.testTag("acctSave")) { Text(tr("Save")) }
        },
        dismissButton = {
            Row {
                onDelete?.let { TextButton(onClick = it, modifier = Modifier.testTag("acctDelete")) { Text(tr("Delete"), color = MaterialTheme.colorScheme.error) } }
                TextButton(onClick = onClose) { Text(tr("Cancel")) }
            }
        },
        title = { Text(if (a == null) tr("New account") else tr("Edit account")) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedTextField(name, { name = it }, singleLine = true, label = { Text(tr("Name")) }, modifier = Modifier.testTag("acctName"))
                FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    for ((k, l) in ACCOUNT_KINDS) FilterChip(kind == k, { kind = k }, label = { Text(tr(l)) }, modifier = Modifier.testTag("acctKind-$k"))
                }
                OutlinedTextField(balance, { balance = it }, singleLine = true, isError = cents == null, keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal),
                    label = { Text(if (kind == "card") tr("How much do you owe on it?") else tr("How much is in it today?")) }, prefix = if (b.currency.isNotEmpty()) ({ Text(b.currency) }) else null, modifier = Modifier.testTag("acctBalance"))
                if (kind != "card") Row(Modifier.fillMaxWidth().heightIn(min = 48.dp).toggleable(overdrawn, role = Role.Switch, onValueChange = { overdrawn = it }).testTag("acctOverdrawn"), verticalAlignment = Alignment.CenterVertically) {
                    Text(tr("Overdrawn (below zero)"), Modifier.weight(1f))
                    Switch(overdrawn, null)
                }
                Text(tr("This changes Not in an envelope yet by the same amount."),
                    style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                onReconcile?.let {
                    OutlinedButton(onClick = it, modifier = Modifier.testTag("reconcileOpen")) {
                        Icon(Ic.CheckCircle, null, Modifier.size(18.dp)); Spacer(Modifier.width(8.dp)); Text(tr("Reconcile with a statement"))
                    }
                }
            }
        },
    )
}

// ---- Month -----------------------------------------------------------------------------------

@Composable
private fun MonthView(env: Env, list: ListItem, b: BudgetView, activity: List<TxItem>) {
    var month by remember(list.id) { mutableStateOf(YearMonth.now()) }
    val from = month.atDay(1).toString()
    val to = month.atEndOfMonth().toString()
    val spend = remember(env.version, list.id, from) { runCatching { env.node.spending(list.id, from, to) }.getOrDefault(emptyList()).sortedByDescending { it.cents } }
    val spent = spend.sumOf { it.cents }
    val got = activity.filter { it.kind == "inc" && it.date in from..to }.sumOf { it.amount }
    val look = LocalLook.current
    val accent = argb(accentColor(look.themeId, list.hue, list.vivid, look.dark))
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(horizontal = 16.dp, vertical = 8.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            IconButton(onClick = { month = month.minusMonths(1) }, modifier = Modifier.testTag("monthPrev")) { Icon(Ic.Back, tr("Previous month")) }
            Text(monthLabel(month), style = MaterialTheme.typography.titleMedium, modifier = Modifier.weight(1f).testTag("monthName"))
            IconButton(onClick = { month = month.plusMonths(1) }, enabled = month < YearMonth.now(), modifier = Modifier.testTag("monthNext")) { Icon(Ic.Right, tr("Next month")) }
        }
        Row {
            Text(tr("Spent in total"), Modifier.weight(1f)); Text(money(spent, b.currency), fontWeight = FontWeight.SemiBold, modifier = Modifier.testTag("monthSpent"))
        }
        Row {
            Text(tr("Received in total"), Modifier.weight(1f)); Text(money(got, b.currency), fontWeight = FontWeight.SemiBold, color = gainColor(), modifier = Modifier.testTag("monthGot"))
        }
        HorizontalDivider()
        if (spend.isEmpty()) Text(tr("Nothing spent this month."), color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.testTag("monthEmpty"))
        val top = spend.maxOfOrNull { it.cents }?.takeIf { it > 0 } ?: 1
        for (s in spend) {
            Column(Modifier.testTag("spend-" + s.envelopeName)) {
                Row {
                    Text(s.envelopeName, Modifier.weight(1f), maxLines = 1, overflow = TextOverflow.Ellipsis)
                    Text(money(s.cents, b.currency) + if (spent > 0) "  ·  ${((s.cents * 100 + spent / 2) / spent)}%" else "")
                }
                Bar((s.cents.toFloat() / top).coerceIn(0f, 1f), accent)
            }
        }
    }
}

// ---- first launch ----------------------------------------------------------------------------

/**
 * First launch: "My budget" asks one thing, how much is in the bank today, then makes the budget
 * with that account and three example envelopes. Nothing is made before, so a fresh install can
 * still restore a backup or link to another device.
 */
@Composable
fun FirstBudget(env: Env, onLinkThis: () -> Unit, onJoin: () -> Unit, onMenu: (() -> Unit)?, open: (ListItem) -> Unit) {
    var text by remember { mutableStateOf("") }
    val cents = if (text.isBlank()) 0L else parseCents(text)
    fun start() {
        val c = cents ?: return
        val node = env.node
        val l = runCatching { node.createList(null, tr("My budget")) }.getOrNull() ?: return
        runCatching { node.setListStyle(l.id, budgetHue(0), 1u, null) }
        runCatching {
            node.setAccount(l.id, null, tr("Bank"), "bank", c)
            // U-S12: no made-up monthly amounts (they meant nothing in another currency); the person sets their own.
            for (name in listOf(tr("Groceries"), tr("Bills"), tr("Fun"))) node.setEnvelope(l.id, null, name, 0L, "m", null, "")
        }
        // U-S12: the currency symbol from the device's country ("$" in Argentina, "€" in Spain); ⋮ › Currency symbol changes it.
        runCatching { java.util.Currency.getInstance(java.util.Locale.getDefault()).getSymbol(java.util.Locale.getDefault()) }.getOrNull()
            ?.takeIf { it.isNotBlank() && it.length <= 8 }
            ?.let { sym -> runCatching { node.setCurrency(l.id, sym) } }
        open(l)
        env.changed()
    }
    Column(Modifier.fillMaxSize()) {
        Row(Modifier.fillMaxWidth().padding(start = 4.dp, top = 8.dp), verticalAlignment = Alignment.CenterVertically) {
            if (onMenu != null) IconButton(onClick = onMenu, modifier = Modifier.testTag("menu")) { Icon(Ic.Menu, tr("Menu")) } else Spacer(Modifier.width(16.dp))
            Text(tr("My budget"), style = MaterialTheme.typography.headlineSmall, fontWeight = FontWeight.SemiBold, modifier = Modifier.testTag("title"))
        }
        Column(Modifier.weight(1f).padding(24.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text(tr("How much is in your bank account today?"), style = MaterialTheme.typography.titleMedium)
            OutlinedTextField(text, { text = it }, singleLine = true, isError = cents == null, keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal),
                placeholder = { Text(zeroAmount()) }, modifier = Modifier.testTag("firstBalance"))
            Button(onClick = ::start, enabled = cents != null, modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp).testTag("firstStart")) { Text(tr("Start my budget")) }
            // U-S1: a partner who installs the app to join a shared budget gets a full-size way in, not only device linking.
            Text(tr("Or"), color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.align(Alignment.CenterHorizontally))
            OutlinedButton(onClick = onJoin, modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp).testTag("firstJoin")) { Text(tr("Join someone's budget")) }
            Spacer(Modifier.weight(1f))
            TextButton(onClick = onLinkThis, modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp).testTag("firstLink")) {
                Text(tr("Link to my own other phone or computer"))
            }
        }
    }
}

/** A new budget's color: green first, then the next of a few distinct hues (changeable in ⋮ › Color). */
fun budgetHue(index: Int): Double = listOf(150.0, 210.0, 30.0, 280.0, 340.0)[Math.floorMod(index, 5)]

/** Adds without wrapping (S3-6: a heading's total of saturated balances must not turn negative). */
internal fun satAdd(a: Long, b: Long): Long {
    val r = a + b
    return if (((a xor r) and (b xor r)) < 0) (if (a < 0) Long.MIN_VALUE else Long.MAX_VALUE) else r
}

/** The shared budget's sync line and whether it is a warning; null before the first sync round has said anything (U-T4). */
internal fun syncLine(s: com.jurisgames.sobres.core.GroupSync, firstRoundDone: Boolean): Pair<String, Boolean>? {
    if (s.onlyHere) return tr("Not sent yet: your newest changes are only on this device.") to true
    if (!firstRoundDone) return null
    val fmt = java.time.format.DateTimeFormatter.ofLocalizedDateTime(java.time.format.FormatStyle.SHORT).withLocale(Lang.locale)
    val others = s.othersLastMs?.let { tr("the others last synced {when}", "when" to fromMillis(it.toLong()).format(fmt)) }
        ?: tr("the others haven't synced yet")
    return tr("Sent") + " · " + others to false
}
