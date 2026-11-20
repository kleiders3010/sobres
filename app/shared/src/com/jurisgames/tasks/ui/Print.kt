package com.jurisgames.tasks.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.AlertDialog
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
import androidx.compose.ui.unit.dp
import com.jurisgames.tasks.core.TaskItem
import java.io.File
import java.time.LocalDateTime
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle

/**
 * Print a list, like Microsoft To Do's "Print list" (support page 'Printing lists in Microsoft To Do': the list's
 * ⋯ menu, switches Print steps and Print notes, then Print list). What's still to do, in the order the list shows it,
 * with a box to tick on paper; steps under their task, notes under them. Long names wrap (To Do cuts them off).
 * Each platform turns the page into a printout ([Env.onPrint]): Android's print service, the desktop's print dialog.
 */
data class PrintChoice(val steps: Boolean = true, val notes: Boolean = true)

private fun choiceFile(dir: String) = File(dir, "ui-print")

/** The last choice, per device (To Do keeps its switches too). */
fun printChoiceOf(dir: String): PrintChoice {
    val s = runCatching { choiceFile(dir).readText() }.getOrDefault("")
    return PrintChoice(steps = "steps=0" !in s, notes = "notes=0" !in s)
}

fun setPrintChoice(dir: String, c: PrintChoice) {
    runCatching { choiceFile(dir).writeText("steps=${if (c.steps) 1 else 0}\nnotes=${if (c.notes) 1 else 0}") }
}

private fun esc(s: String) = s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;").replace("\"", "&quot;")

/**
 * The page to print: the list's name, the date, then its open tasks as [sections] orders them on screen.
 * Steps keep their own tick (a done step prints ticked); hidden steps hide their own steps too.
 */
fun printHtml(name: String, tasks: List<TaskItem>, sort: Sort, choice: PrintChoice, now: LocalDateTime = LocalDateTime.now()): String {
    val rows = sections(tasks, emptySet(), sort = sort).active.filter { choice.steps || it.parentId == null }
    val date = now.format(DateTimeFormatter.ofLocalizedDate(FormatStyle.LONG).withLocale(Lang.locale))
    val b = StringBuilder()
    b.append("<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>").append(esc(name)).append("</title><style>")
    b.append("@page{margin:15mm}body{font-family:sans-serif;font-size:12pt;color:#000;margin:0}")
    b.append("h1{font-size:18pt;margin:0 0 2pt}.date{color:#555;font-size:10pt;margin:0 0 12pt}")
    b.append("ul{list-style:none;margin:0;padding:0}li{font-weight:normal;margin:0 0 6pt;padding-left:22pt;text-indent:-22pt;overflow-wrap:anywhere;word-wrap:break-word;page-break-inside:avoid}")
    b.append(".step{margin-left:22pt}.box{display:inline-block;width:16pt;text-indent:0}")
    b.append(".note{color:#333;font-size:10pt;text-indent:0;margin:2pt 0 0}.when{color:#555;font-size:10pt}")
    b.append(".empty{color:#555}")
    b.append("</style></head><body><h1>").append(esc(name)).append("</h1><p class=\"date\">").append(esc(date)).append("</p>")
    if (rows.isEmpty()) b.append("<p class=\"empty\">").append(esc(tr("Nothing to do"))).append("</p>")
    else {
        b.append("<ul>")
        for (t in rows) {
            val step = t.parentId != null
            b.append("<li class=\"").append(if (step) "step" else "task").append("\"")
            if (step && t.depth > 1u) b.append(" style=\"margin-left:${22 * t.depth.toInt()}pt\"")
            b.append("><span class=\"box\">").append(if (t.done) "☑" else "☐").append("&nbsp;</span> ")
            b.append(esc(t.title.trim().ifEmpty { tr("(untitled)") }))
            if (t.starredMs != null) b.append(" ★")
            t.remindMs?.let { ms ->
                b.append(" <span class=\"when\">· ").append(if (t.repeat != null) "↻ " else "").append(esc(whenLabel(fromMillis(ms.toLong()), now))).append("</span>")
            }
            val note = t.notes.trim()
            if (choice.notes && note.isNotEmpty()) b.append("<div class=\"note\">").append(note.lines().joinToString("<br>") { esc(it) }).append("</div>")
            b.append("</li>")
        }
        b.append("</ul>")
    }
    b.append("</body></html>")
    return b.toString()
}

/** The ⋯ menu's Print list: the two switches, then Print. */
@Composable
fun PrintDialog(dataDir: String, onPrint: (PrintChoice) -> Unit, onDismiss: () -> Unit) {
    var choice by remember { mutableStateOf(printChoiceOf(dataDir)) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(tr("Print list")) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                PrintSwitch(tr("Print steps"), choice.steps, "printSteps") { choice = choice.copy(steps = it) }
                PrintSwitch(tr("Print notes"), choice.notes, "printNotes") { choice = choice.copy(notes = it) }
            }
        },
        confirmButton = {
            TextButton(onClick = { setPrintChoice(dataDir, choice); onDismiss(); onPrint(choice) }, modifier = Modifier.testTag("printGo")) { Text(tr("Print")) }
        },
        dismissButton = { TextButton(onClick = onDismiss, modifier = Modifier.testTag("printCancel")) { Text(tr("Cancel")) } },
        modifier = Modifier.testTag("printDialog"),
    )
}

@Composable
private fun PrintSwitch(label: String, value: Boolean, tag: String, onChange: (Boolean) -> Unit) {
    Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
        Text(label, modifier = Modifier.weight(1f))
        Switch(value, onChange, modifier = Modifier.testTag(tag))
    }
}
