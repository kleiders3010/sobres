package com.jurisgames.sobres

import android.app.Activity
import android.content.Context
import android.content.Intent
import androidx.activity.ComponentActivity
import androidx.activity.result.ActivityResultLauncher
import androidx.activity.result.contract.ActivityResultContract

/** foss flavor: our own camera screen ([ScanActivity]) with the ZXing decoder. */
object QrScanner {
    private class Scan : ActivityResultContract<Unit, String?>() {
        override fun createIntent(context: Context, input: Unit) = Intent(context, ScanActivity::class.java)
        override fun parseResult(resultCode: Int, intent: Intent?) = intent?.getStringExtra("text").takeIf { resultCode == Activity.RESULT_OK }
    }

    /** Nothing to download: the camera scanner is part of the app. */
    fun prepare(context: Context) {}

    fun scan(activity: ComponentActivity, onResult: (String) -> Unit) {
        var launcher: ActivityResultLauncher<Unit>? = null
        launcher = activity.activityResultRegistry.register("qr-scan-" + System.nanoTime(), Scan()) { text ->
            launcher?.unregister()
            text?.let(onResult)
        }
        launcher.launch(Unit)
    }
}
