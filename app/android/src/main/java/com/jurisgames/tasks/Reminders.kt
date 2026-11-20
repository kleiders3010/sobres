package com.jurisgames.tasks

import com.jurisgames.tasks.ui.tr
import android.app.AlarmManager
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.os.Build

/**
 * Task reminders (§12, D43): an exact alarm per upcoming reminder when the app may set
 * them (SCHEDULE_EXACT_ALARM), an inexact one otherwise (never lost, maybe late).
 * Rescheduled after syncs, when leaving the app, at boot and when the permission is granted.
 */
object Reminders {
    private const val PREFS = "reminders"
    const val CHANNEL = "reminders"

    fun canBeExact(ctx: Context): Boolean =
        Build.VERSION.SDK_INT < 31 || ctx.getSystemService(AlarmManager::class.java).canScheduleExactAlarms()

    /** Replace all scheduled alarms with the current upcoming reminders. Returns how many. */
    fun reschedule(ctx: Context): Int {
        val app = ctx.applicationContext as TasksApplication
        val am = ctx.getSystemService(AlarmManager::class.java)
        val prefs = ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        for (key in prefs.getStringSet("scheduled", emptySet()).orEmpty()) {
            am.cancel(pending(ctx, key, "", ""))
        }
        val now = System.currentTimeMillis()
        val keys = mutableSetOf<String>()
        for (r in app.node.upcomingReminders()) {
            val key = r.listId + "|" + r.taskId
            val at = maxOf(r.atMs.toLong(), now + 1_000)
            val pi = pending(ctx, key, r.title, r.listName)
            if (canBeExact(ctx)) am.setExactAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, at, pi)
            else am.setAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, at, pi)
            keys += key
        }
        prefs.edit().putStringSet("scheduled", keys).apply()
        return keys.size
    }

    private fun pending(ctx: Context, key: String, title: String, list: String): PendingIntent {
        val i = Intent(ctx, ReminderReceiver::class.java).setAction("com.jurisgames.tasks.REMIND").putExtra("key", key).putExtra("title", title).putExtra("list", list)
        return PendingIntent.getBroadcast(ctx, key.hashCode(), i, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
    }

    fun show(ctx: Context, key: String, title: String, list: String) {
        val nm = ctx.getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(NotificationChannel(CHANNEL, tr("Reminders"), NotificationManager.IMPORTANCE_HIGH))
        val open = PendingIntent.getActivity(ctx, 0, Intent(ctx, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE)
        val n = Notification.Builder(ctx, CHANNEL)
            .setSmallIcon(android.R.drawable.ic_popup_reminder)
            .setContentTitle(title)
            .setContentText(list)
            .setContentIntent(open)
            .setAutoCancel(true)
            .build()
        nm.notify(("r" + key).hashCode(), n)
    }
}

/** An alarm went off: show the reminder. */
class ReminderReceiver : BroadcastReceiver() {
    override fun onReceive(ctx: Context, intent: Intent) {
        Reminders.show(ctx, intent.getStringExtra("key").orEmpty(), intent.getStringExtra("title").orEmpty(), intent.getStringExtra("list").orEmpty())
    }
}

/** Boot, app update, or the exact-alarm permission granted: alarms need setting again. */
class RescheduleReceiver : BroadcastReceiver() {
    override fun onReceive(ctx: Context, intent: Intent) {
        Measure.ensureAlarm(ctx) // the "more often" measurement alarm, if on
        val done = goAsync()
        Thread {
            try {
                runCatching { Reminders.reschedule(ctx) }
            } finally {
                done.finish()
            }
        }.start()
    }
}
