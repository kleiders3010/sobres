package com.jurisgames.tasks

import android.app.Application
import androidx.work.Constraints
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.NetworkType
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import com.jurisgames.tasks.core.KtNode
import com.jurisgames.tasks.core.NodeOptions
import com.jurisgames.tasks.core.Platform
import java.io.File
import java.util.concurrent.TimeUnit

class TasksApplication : Application() {
    private var current: KtNode? = null

    /** One node per process; opened on first use (UI or background job). */
    val node: KtNode
        @Synchronized get() = current ?: open(null).also { current = it; Measure.onNodeOpened(this, it) }

    /** The node if something already opened it (never opens it). */
    val nodeIfOpen: KtNode?
        @Synchronized get() = current

    /** Registers the app Context with the Rust side (src/android.rs in kt-ffi). */
    private external fun initAndroidContext(context: android.content.Context)

    private fun open(bytes: ByteArray?): KtNode {
        // Same library JNA uses; loading it this way lets it see the app Context, which
        // iroh needs for the system DNS servers and network changes.
        System.loadLibrary("kt_ffi")
        initAndroidContext(this)
        com.jurisgames.tasks.core.setKeyVault(KeystoreVault) // device keys sealed by the Keystore (§9)
        val identity = File(noBackupFilesDir, "identity") // never backed up (D1)
        val data = File(filesDir, "data") // not backed up either: rebuilt from the backup file
        identity.mkdirs()
        data.mkdirs()
        // The import sandbox worker ships as a native "library" so Android lets us run it.
        val worker = File(applicationInfo.nativeLibraryDir, "libkt_import.so").absolutePath
        // The only thing Android backup takes (res/xml/*backup*): restored on a new phone
        // before the first launch, and then picked up by KtNode.open automatically.
        val backup = File(filesDir, "backup").absolutePath
        val options = NodeOptions(Platform.PHONE, emptyList(), true, true, "", android.os.Build.MODEL, backup)
        return if (bytes == null) KtNode.open(identity.absolutePath, data.absolutePath, worker, options)
        else KtNode.openRestoring(identity.absolutePath, data.absolutePath, worker, options, bytes)
    }

    /** First launch, restore skipped during phone setup: restore a backup file the user picked. */
    @Synchronized
    fun restoreFrom(bytes: ByteArray): KtNode {
        current?.let { it.stopLive(); it.close() }
        current = null
        return open(bytes).also { current = it; Measure.onNodeOpened(this, it) }
    }

    /** Wi-Fi and charging decide whether this phone helps others sync right now (§7a). */
    fun reportDeviceState() {
        val cm = getSystemService(android.net.ConnectivityManager::class.java)
        val wifi = cm.getNetworkCapabilities(cm.activeNetwork)?.let {
            it.hasTransport(android.net.NetworkCapabilities.TRANSPORT_WIFI) || it.hasTransport(android.net.NetworkCapabilities.TRANSPORT_ETHERNET)
        } ?: false
        val battery = getSystemService(android.os.BatteryManager::class.java)
        val power = getSystemService(android.os.PowerManager::class.java)
        val lowBattery = battery.getIntProperty(android.os.BatteryManager.BATTERY_PROPERTY_CAPACITY).let { it in 0..19 } && !battery.isCharging
        val hot = android.os.Build.VERSION.SDK_INT >= 29 && power.currentThermalStatus >= android.os.PowerManager.THERMAL_STATUS_SEVERE
        val dataSaver = cm.restrictBackgroundStatus == android.net.ConnectivityManager.RESTRICT_BACKGROUND_STATUS_ENABLED
        // No helping others in battery saver, low battery, heat or data saver (§7a).
        node.setDeviceState(wifi, battery.isCharging, power.isPowerSaveMode || lowBattery || hot || dataSaver)
    }

    /** "Sync in the background": periodic work on or off. */
    fun applyBackgroundSync(on: Boolean) {
        if (on) schedulePeriodic() else WorkManager.getInstance(this).cancelUniqueWork("sync")
    }

    override fun onCreate() {
        super.onCreate()
        // Network changes go to P2P right away (§8): it re-checks paths instead of waiting.
        getSystemService(android.net.ConnectivityManager::class.java).registerDefaultNetworkCallback(object : android.net.ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: android.net.Network) = changed()
            override fun onLost(network: android.net.Network) = changed()
            override fun onCapabilitiesChanged(network: android.net.Network, caps: android.net.NetworkCapabilities) = changed()
            private fun changed() {
                if (current == null) return // nothing to tell before the node exists
                Measure.networkChanged()
                runCatching { reportDeviceState(); node.networkChanged() }
            }
        })
        schedulePeriodic() // the worker itself checks "Sync in the background"
        Measure.ensureAlarm(this) // measurement only; off unless switched on over adb
    }

    private fun schedulePeriodic() {
        // Background sync: WorkManager's 15-minute minimum; Doze stretches it (DESIGN §8).
        val periodic = PeriodicWorkRequestBuilder<SyncWorker>(15, TimeUnit.MINUTES)
            .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
            .setInputData(androidx.work.workDataOf(SyncWorker.PERIODIC to true))
            .build()
        WorkManager.getInstance(this).enqueueUniquePeriodicWork("sync", ExistingPeriodicWorkPolicy.UPDATE, periodic)
    }
}
