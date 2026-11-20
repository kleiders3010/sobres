package com.jurisgames.sobres

import android.app.AlarmManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.os.SystemClock
import android.util.Log
import androidx.work.ExistingWorkPolicy
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.OutOfQuotaPolicy
import androidx.work.WorkManager
import androidx.work.workDataOf

/**
 * MEASUREMENT ONLY, not a feature: the switches for the two pending battery measurements,
 * set from `adb shell` by scripts/battery-measure.sh. Nothing in the app's UI reaches them,
 * and both are off by default.
 *
 * - **"More often" checks** (D47, DESIGN §8): a silent, inexact `setAndAllowWhileIdle`
 *   alarm every few minutes (default 20) runs one normal sync round (SyncWorker,
 *   expedited, like leaving the app). No notification, no exact alarm, nothing that
 *   overrides Do Not Disturb. It reschedules itself and survives reboot and app updates.
 * - **Extra network endpoints** (D23): N-1 idle iroh endpoints next to the real one
 *   (`KtNode.measureExtraEndpoints`), pricing one network identity per group for N groups.
 *
 * Switched by [MeasureReceiver], which only senders holding `android.permission.DUMP` can
 * reach: the adb shell holds it, and apps can't get it (signature|privileged|development).
 * This works on a release-signed app, where `run-as` doesn't.
 */
object Measure {
    const val TAG = "KtMeasure"
    private const val PREFS = "measure"
    const val DEFAULT_MINUTES = 20
    private const val ALARM_ACTION = "com.jurisgames.sobres.MORE_OFTEN"

    private fun prefs(ctx: Context) = ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    /**
     * Network-change nudges passed to the node since the scenario was set (in this process):
     * each one makes every endpoint re-check its paths, so N endpoints pay it N times.
     */
    private val netChanges = java.util.concurrent.atomic.AtomicInteger()

    fun networkChanged() {
        netChanges.incrementAndGet()
    }

    /** Minutes between "more often" checks; 0 = off. */
    fun minutes(ctx: Context): Int = prefs(ctx).getInt("more_often_min", 0)

    /** Network endpoints to keep (the real one + extras); 0 or 1 = no extras. */
    fun endpoints(ctx: Context): Int = prefs(ctx).getInt("endpoints", 1)

    private fun alarmIntent(ctx: Context, flags: Int): PendingIntent? {
        val i = Intent(ctx, MoreOftenReceiver::class.java).setAction(ALARM_ACTION)
        return PendingIntent.getBroadcast(ctx, 0x6d6f, i, PendingIntent.FLAG_IMMUTABLE or flags)
    }

    /** The next check, `minutes` from now (replaces any pending one). Inexact on purpose. */
    fun scheduleNext(ctx: Context) {
        val min = minutes(ctx)
        val am = ctx.getSystemService(AlarmManager::class.java)
        if (min <= 0) {
            alarmIntent(ctx, PendingIntent.FLAG_NO_CREATE)?.let { am.cancel(it); it.cancel() }
            return
        }
        val at = SystemClock.elapsedRealtime() + min * 60_000L
        am.setAndAllowWhileIdle(AlarmManager.ELAPSED_REALTIME_WAKEUP, at, alarmIntent(ctx, PendingIntent.FLAG_UPDATE_CURRENT)!!)
        prefs(ctx).edit().putLong("next_at", at).apply()
    }

    /**
     * App start, boot or update: make sure a check is pending if the mode is on. An alarm
     * that is still pending is left alone, so restarts of the process don't push it back.
     * Lost ones are set again: none recorded, a time from before a reboot (the clock
     * restarted), or hours overdue (app standby defers alarms, but not that long).
     */
    fun ensureAlarm(ctx: Context) {
        val min = minutes(ctx)
        if (min <= 0) return
        val next = prefs(ctx).getLong("next_at", 0)
        val now = SystemClock.elapsedRealtime()
        val lost = next == 0L || next > now + min * 60_000L + 60_000 || now > next + 3 * 3600_000L
        if (lost || alarmIntent(ctx, PendingIntent.FLAG_NO_CREATE) == null) scheduleNext(ctx)
    }

    /** The node just opened (any process start): start the extra endpoints if asked. */
    fun onNodeOpened(ctx: Context, node: com.jurisgames.sobres.core.KtNode) {
        val extra = endpoints(ctx) - 1
        if (extra <= 0) return
        Thread {
            val n = runCatching { node.measureExtraEndpoints(extra.toUInt()) }.getOrNull()
            Log.i(TAG, "extra endpoints started: $n")
        }.start()
    }

    /** An alarm fired: count it, set the next one, and run one normal sync round. */
    fun fired(ctx: Context) {
        prefs(ctx).edit().putInt("alarms", prefs(ctx).getInt("alarms", 0) + 1).putLong("last_alarm", System.currentTimeMillis()).apply()
        scheduleNext(ctx) // first, so a failing round never ends the chain
        val req = OneTimeWorkRequestBuilder<SyncWorker>()
            .setExpedited(OutOfQuotaPolicy.RUN_AS_NON_EXPEDITED_WORK_REQUEST)
            .setInputData(workDataOf(SyncWorker.MORE_OFTEN to true))
            .build()
        // KEEP: a round still waiting (no network yet, out of quota) isn't doubled.
        WorkManager.getInstance(ctx).enqueueUniqueWork("more-often", ExistingWorkPolicy.KEEP, req)
        Log.i(TAG, "more-often alarm fired; round enqueued")
    }

    /** A round started by the alarm finished (SyncWorker). */
    fun roundDone(ctx: Context, ms: Long) {
        val p = prefs(ctx)
        p.edit().putInt("rounds", p.getInt("rounds", 0) + 1).putLong("round_ms_total", p.getLong("round_ms_total", 0) + ms).apply()
        Log.i(TAG, "more-often round done in $ms ms")
    }

    /** Set the scenario. Counters restart with every change. */
    fun set(ctx: Context, minutes: Int, endpoints: Int) {
        prefs(ctx).edit().clear().putInt("more_often_min", minutes.coerceAtLeast(0)).putInt("endpoints", endpoints.coerceIn(1, 65)).commit()
        netChanges.set(0)
        scheduleNext(ctx)
    }

    /** One line of `key=value` pairs (the script parses it). */
    fun status(ctx: Context): String {
        val p = prefs(ctx)
        val app = ctx.applicationContext as TasksApplication
        val node = app.nodeIfOpen
        val next = p.getLong("next_at", 0)
        val pending = alarmIntent(ctx, PendingIntent.FLAG_NO_CREATE) != null
        return listOf(
            "more_often_min=${minutes(ctx)}",
            "endpoints=${endpoints(ctx)}",
            "sync_bg=${node?.settings()?.syncInBackground ?: "unknown"}",
            "extra_running=${if (endpoints(ctx) > 1) node?.measureEndpointsRunning() ?: 0u else 0u}",
            "extra_online=${if (endpoints(ctx) > 1) node?.measureEndpointsOnline() ?: 0u else 0u}",
            "alarm_pending=$pending",
            "next_alarm_in_s=${if (pending && next > 0) (next - SystemClock.elapsedRealtime()) / 1000 else -1}",
            "alarms=${p.getInt("alarms", 0)}",
            "rounds=${p.getInt("rounds", 0)}",
            "round_ms_total=${p.getLong("round_ms_total", 0)}",
            "net_changes=${netChanges.get()}",
        ).joinToString(" ")
    }
}

/** The "more often" alarm (not exported: only AlarmManager fires it). */
class MoreOftenReceiver : BroadcastReceiver() {
    override fun onReceive(ctx: Context, intent: Intent) = Measure.fired(ctx)
}

/**
 * The adb measurement switch (see [Measure]). Protected by `android.permission.DUMP`, so
 * only `adb shell` can send it:
 *
 *     adb shell am broadcast -n com.jurisgames.sobres/.MeasureReceiver \
 *         -a com.jurisgames.sobres.MEASURE --ei more_often_min 20 --ei endpoints 1
 *
 * Without extras it only reports. The reply (the broadcast's result data) is the status line.
 */
class MeasureReceiver : BroadcastReceiver() {
    override fun onReceive(ctx: Context, intent: Intent) {
        val done = goAsync()
        Thread {
            try {
                val app = ctx.applicationContext as TasksApplication
                if (intent.hasExtra("more_often_min") || intent.hasExtra("endpoints")) {
                    val min = intent.getIntExtra("more_often_min", Measure.minutes(ctx))
                    val n = intent.getIntExtra("endpoints", Measure.endpoints(ctx))
                    Measure.set(ctx, min, n)
                    // Open the node now (as any wake would) and match the extra endpoints.
                    val started = app.node.measureExtraEndpoints((Measure.endpoints(ctx) - 1).toUInt())
                    Log.i(Measure.TAG, "scenario: more_often_min=$min endpoints=$n (extra running: $started)")
                }
                done.resultCode = 1
                done.resultData = Measure.status(ctx)
            } catch (e: Throwable) {
                done.resultCode = 2
                done.resultData = "error=${e.javaClass.simpleName}: ${e.message}"
            } finally {
                done.finish()
            }
        }.start()
    }
}
