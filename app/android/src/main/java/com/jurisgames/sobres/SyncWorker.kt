package com.jurisgames.sobres

import android.content.Context
import androidx.work.CoroutineWorker
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.OutOfQuotaPolicy
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import androidx.work.ExistingWorkPolicy

/** One sync round on every layer (periodic, and expedited when leaving the app). */
class SyncWorker(ctx: Context, params: WorkerParameters) : CoroutineWorker(ctx, params) {
    override suspend fun doWork(): Result {
        val app = applicationContext as TasksApplication
        val node = app.node
        if (!node.settings().syncInBackground) return Result.success() // sync only while open
        app.reportDeviceState()
        val started = android.os.SystemClock.elapsedRealtime()
        node.flush(10_000uL) // what this device hasn't sent yet goes first, even if the job is cut short
        val report = node.syncNow()
        ChangeNotifier.joined(applicationContext, report.joinedGroups, report.lookupsGivenUp.toInt())
        if (inputData.getBoolean(MORE_OFTEN, false)) Measure.roundDone(applicationContext, android.os.SystemClock.elapsedRealtime() - started)
        if (inputData.getBoolean(PERIODIC, false)) BackgroundCheck.ran(applicationContext)
        ChangeNotifier.post(applicationContext, node.takeChanges())
        return Result.success()
    }

    companion object {
        const val PERIODIC = "periodic"
        /** Started by the "more often" alarm (measurement only, [Measure]). */
        const val MORE_OFTEN = "more_often"

        /** Leaving the app: send pending changes now (expedited, no notification; DESIGN §8). */
        fun flushNow(ctx: Context) {
            val req = OneTimeWorkRequestBuilder<SyncWorker>().setExpedited(OutOfQuotaPolicy.RUN_AS_NON_EXPEDITED_WORK_REQUEST).build()
            WorkManager.getInstance(ctx).enqueueUniqueWork("flush", ExistingWorkPolicy.REPLACE, req)
        }
    }
}
