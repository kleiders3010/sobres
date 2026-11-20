package com.jurisgames.sobres

import android.app.ActivityManager
import android.app.usage.UsageStatsManager
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.SystemClock
import android.provider.Settings
import java.io.File

/**
 * Blocked background sync (DESIGN §8): some phones (aggressive OEM battery savers, or the
 * user's own "Restricted" setting) never run background work. Detected, explained once,
 * with a button to the app's settings. The battery exemption is never requested directly.
 */
object BackgroundCheck {
    private const val DAY_MS = 24 * 3600 * 1000L

    /** A periodic background sync ran (SyncWorker). */
    fun ran(ctx: Context) {
        File(ctx.noBackupFilesDir, "last_background_sync").writeText(System.currentTimeMillis().toString())
    }

    fun blocked(ctx: Context): Boolean {
        val app = ctx.applicationContext as TasksApplication
        if (!app.node.settings().syncInBackground) return false
        if (Build.VERSION.SDK_INT >= 28 && ctx.getSystemService(ActivityManager::class.java).isBackgroundRestricted) return true
        if (Build.VERSION.SDK_INT >= 30 &&
            ctx.getSystemService(UsageStatsManager::class.java).appStandbyBucket == UsageStatsManager.STANDBY_BUCKET_RESTRICTED
        ) return true
        // Starved: on for more than a day, yet no periodic sync in two days (installed or
        // last run), though the job asks for one every 15 minutes, stretched by Doze to hours.
        val last = File(ctx.noBackupFilesDir, "last_background_sync").takeIf { it.exists() }?.readText()?.toLongOrNull()
        val since = maxOf(last ?: 0L, ctx.packageManager.getPackageInfo(ctx.packageName, 0).firstInstallTime)
        return SystemClock.elapsedRealtime() > DAY_MS && System.currentTimeMillis() - since > 2 * DAY_MS
    }

    /** Explained once per install. */
    fun shouldExplain(ctx: Context): Boolean {
        val marker = File(ctx.noBackupFilesDir, "background_explained")
        if (marker.exists() || !blocked(ctx)) return false
        marker.writeText("")
        return true
    }

    /** The app's own settings page, where battery use / background activity is changed. */
    fun openSettings(ctx: Context) {
        ctx.startActivity(Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.parse("package:${ctx.packageName}")))
    }
}
