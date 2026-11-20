package com.jurisgames.tasks

import com.jurisgames.tasks.ui.tr
import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import com.jurisgames.tasks.core.ChangeItem

/**
 * Notifications about others' changes (DESIGN §8): one per list, batched by person
 * ("Sam added milk, eggs and completed bread"), timed at the latest change so the age shows.
 * The node already dropped stale changes and our own.
 */
object ChangeNotifier {
    const val CHANNEL = "changes"

    fun post(ctx: Context, changes: List<ChangeItem>) {
        if (changes.isEmpty()) return
        if (Build.VERSION.SDK_INT >= 33 && ctx.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) return
        val nm = ctx.getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(NotificationChannel(CHANNEL, tr("Changes by others"), NotificationManager.IMPORTANCE_DEFAULT))
        val open = PendingIntent.getActivity(ctx, 0, Intent(ctx, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE)
        // One per list; joins one per group.
        for ((key, cs) in changes.groupBy { it.listId.ifEmpty { "group:" + it.groupName } }) {
            val text = describe(cs)
            val n = Notification.Builder(ctx, CHANNEL)
                .setSmallIcon(android.R.drawable.checkbox_on_background)
                .setContentTitle(cs.first().listName.ifEmpty { cs.first().groupName })
                .setContentText(text)
                .setStyle(Notification.BigTextStyle().bigText(text))
                .setWhen(cs.maxOf { it.atMs.toLong() })
                .setShowWhen(true)
                .setContentIntent(open)
                .setAutoCancel(true)
                .build()
            nm.notify(key.hashCode(), n)
        }
    }

    /**
     * Our own joins that finished in the background (the code was still being looked for,
     * or a member's device had to let us in), and codes given up on after a day.
     * Placeholder wording (UX pending).
     */
    fun joined(ctx: Context, groups: List<String>, gaveUp: Int) {
        if (groups.isEmpty() && gaveUp == 0) return
        if (Build.VERSION.SDK_INT >= 33 && ctx.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) return
        val nm = ctx.getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(NotificationChannel(JOINS, tr("Joining"), NotificationManager.IMPORTANCE_DEFAULT))
        val open = PendingIntent.getActivity(ctx, 0, Intent(ctx, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE)
        fun post(id: Int, title: String, text: String) = nm.notify(id, Notification.Builder(ctx, JOINS)
            .setSmallIcon(android.R.drawable.checkbox_on_background)
            .setContentTitle(title).setContentText(text)
            .setContentIntent(open).setAutoCancel(true).build())
        for (g in groups) post(("joined:" + g).hashCode(), tr("You're in {group}", "group" to g), tr("Its lists are on this phone now."))
        if (gaveUp > 0) post("gave-up".hashCode(), tr("Couldn't find that invite"), tr("Check the code with the person who sent it, or ask for a new one."))
    }

    private const val JOINS = "joins"

    /** "Sam added milk, eggs and completed bread; Alex added apples", "Kim joined". */
    fun describe(cs: List<ChangeItem>): String =
        cs.groupBy { it.who }.entries.joinToString("; ") { (who, xs) ->
            who + " " + xs.groupBy { it.kind }.entries.joinToString(tr(" and ")) { (kind, ys) ->
                if (kind == "joined") tr("joined") else tr(kind) + " " + ys.joinToString(", ") { it.title }
            }
        }
}
