package com.jurisgames.tasks

import android.app.NotificationManager
import android.content.Intent
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Test
import org.junit.runner.RunWith

/** Reminders: set on a task, scheduled as an alarm, shown when it fires. */
@RunWith(AndroidJUnit4::class)
class ReminderTest {
    @Test
    fun aReminderIsScheduledAndShown() {
        val inst = InstrumentationRegistry.getInstrumentation()
        val ctx = inst.targetContext
        if (android.os.Build.VERSION.SDK_INT >= 33) {
            inst.uiAutomation.grantRuntimePermission(ctx.packageName, android.Manifest.permission.POST_NOTIFICATIONS)
        }
        val node = (ctx.applicationContext as TasksApplication).node
        val list = node.createList(null, "Reminded")
        val task = node.addTask(list.id, "call the dentist", null)
        node.setReminder(list.id, task, (System.currentTimeMillis() + 3_600_000).toULong())
        check(node.upcomingReminders().any { it.taskId == task }) { "listed as upcoming" }
        check(Reminders.reschedule(ctx) >= 1) { "an alarm was set" }

        // The alarm firing (its broadcast), without waiting an hour.
        ReminderReceiver().onReceive(ctx, Intent().putExtra("key", "${list.id}|$task").putExtra("title", "call the dentist").putExtra("list", "Reminded"))
        val nm = ctx.getSystemService(NotificationManager::class.java)
        val deadline = System.currentTimeMillis() + 10_000
        while (nm.activeNotifications.none { it.notification.extras.getString("android.title") == "call the dentist" } && System.currentTimeMillis() < deadline) Thread.sleep(100)
        check(nm.activeNotifications.any { it.notification.extras.getString("android.title") == "call the dentist" }) { "shown" }

        // Cleared: no longer upcoming.
        node.setReminder(list.id, task, null)
        check(node.upcomingReminders().none { it.taskId == task })
    }
}
