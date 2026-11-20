package com.jurisgames.tasks

import com.jurisgames.tasks.ui.tr
import android.content.Context
import androidx.compose.runtime.Composable
import androidx.compose.ui.unit.dp
import androidx.glance.GlanceId
import androidx.glance.GlanceModifier
import androidx.glance.GlanceTheme
import androidx.glance.action.ActionParameters
import androidx.glance.action.actionParametersOf
import androidx.glance.action.clickable
import androidx.glance.appwidget.CheckBox
import androidx.glance.appwidget.GlanceAppWidget
import androidx.glance.appwidget.GlanceAppWidgetReceiver
import androidx.glance.appwidget.action.ActionCallback
import androidx.glance.appwidget.action.actionRunCallback
import androidx.glance.appwidget.lazy.LazyColumn
import androidx.glance.appwidget.lazy.items
import androidx.glance.appwidget.provideContent
import androidx.glance.appwidget.updateAll
import androidx.glance.background
import androidx.glance.layout.Alignment
import androidx.glance.layout.Column
import androidx.glance.layout.Row
import androidx.glance.layout.fillMaxSize
import androidx.glance.layout.fillMaxWidth
import androidx.glance.layout.padding
import androidx.glance.semantics.semantics
import androidx.glance.semantics.testTag
import androidx.glance.text.FontWeight
import androidx.glance.text.Text
import androidx.glance.text.TextStyle
import java.text.DateFormat
import java.util.Date

/** What the widget shows (built from the node, or by tests). */
data class WidgetTask(val id: String, val title: String, val done: Boolean, val depth: Int)
data class WidgetState(val listId: String?, val listName: String, val tasks: List<WidgetTask>, val updatedMs: Long)

/**
 * Home-screen widget (DESIGN §8): one list with checkboxes (backported below Android 12 by
 * Glance). Tapping the title shows the next list. Redrawn after every change the app or a
 * background sync makes; the time shown is absolute ("Updated 14:05"), since a relative
 * label would freeze between redraws.
 */
class TasksWidget : GlanceAppWidget() {
    override suspend fun provideGlance(context: Context, id: GlanceId) {
        val state = load(context)
        provideContent { GlanceTheme { WidgetContent(state) } }
    }

    companion object {
        private const val PREFS = "widget"

        /** Redraw every placed widget (after a change or a sync). */
        suspend fun refresh(context: Context) = TasksWidget().updateAll(context)

        fun load(context: Context): WidgetState {
            val node = (context.applicationContext as TasksApplication).node
            val lists = node.lists()
            val chosen = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString("list", null)
            val list = lists.firstOrNull { it.id == chosen } ?: lists.firstOrNull()
            val tasks = list?.let { l -> runCatching { node.tasks(l.id) }.getOrDefault(emptyList()) }.orEmpty()
            return WidgetState(list?.id, list?.name ?: tr("No lists yet"), tasks.map { WidgetTask(it.id, it.title, it.done, it.depth.toInt()) }, System.currentTimeMillis())
        }

        fun choose(context: Context, listId: String) {
            context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit().putString("list", listId).apply()
        }
    }
}

val TaskIdKey = ActionParameters.Key<String>("task")
val ListIdKey = ActionParameters.Key<String>("list")
val DoneKey = ActionParameters.Key<Boolean>("done")

@Composable
fun WidgetContent(state: WidgetState) {
    Column(GlanceModifier.fillMaxSize().background(GlanceTheme.colors.widgetBackground).padding(12.dp)) {
        Row(GlanceModifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Text(
                state.listName,
                style = TextStyle(fontWeight = FontWeight.Bold, color = GlanceTheme.colors.onSurface),
                modifier = GlanceModifier.defaultWeight().clickable(actionRunCallback<NextList>()).semantics { testTag = "widgetTitle" },
            )
            Text(tr("Updated {time}", "time" to DateFormat.getTimeInstance(DateFormat.SHORT).format(Date(state.updatedMs))), style = TextStyle(color = GlanceTheme.colors.onSurfaceVariant))
        }
        LazyColumn {
            items(state.tasks, itemId = { it.id.hashCode().toLong() }) { t ->
                CheckBox(
                    checked = t.done,
                    onCheckedChange = actionRunCallback<ToggleTask>(actionParametersOf(TaskIdKey to t.id, ListIdKey to (state.listId ?: ""), DoneKey to !t.done)),
                    text = t.title,
                    modifier = GlanceModifier.padding(start = (t.depth * 16).dp).semantics { testTag = "wtask-" + t.title },
                )
            }
        }
    }
}

/** A checkbox tap: straight into the engine, then an expedited sync and a redraw. */
class ToggleTask : ActionCallback {
    override suspend fun onAction(context: Context, glanceId: GlanceId, parameters: ActionParameters) {
        val node = (context.applicationContext as TasksApplication).node
        val listId = parameters[ListIdKey]!!
        val taskId = parameters[TaskIdKey]!!
        val task = runCatching { node.tasks(listId).firstOrNull { it.id == taskId } }.getOrNull()
        // a repeating task moves to its next date instead of being done
        if (task != null) com.jurisgames.tasks.ui.tickTask(node, listId, task, parameters[DoneKey]!!)
        else node.setDone(listId, taskId, parameters[DoneKey]!!)
        SyncWorker.flushNow(context)
        TasksWidget.refresh(context)
    }
}

/** Title tap: show the next list. */
class NextList : ActionCallback {
    override suspend fun onAction(context: Context, glanceId: GlanceId, parameters: ActionParameters) {
        val lists = (context.applicationContext as TasksApplication).node.lists()
        if (lists.isEmpty()) return
        val current = TasksWidget.load(context).listId
        val next = lists[(lists.indexOfFirst { it.id == current } + 1).mod(lists.size)]
        TasksWidget.choose(context, next.id)
        TasksWidget.refresh(context)
    }
}

class TasksWidgetReceiver : GlanceAppWidgetReceiver() {
    override val glanceAppWidget: GlanceAppWidget = TasksWidget()
}
