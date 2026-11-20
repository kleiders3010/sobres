package com.jurisgames.tasks

import android.content.Context
import android.widget.Toast
import androidx.activity.ComponentActivity
import com.google.android.gms.common.moduleinstall.ModuleInstall
import com.google.android.gms.common.moduleinstall.ModuleInstallRequest
import com.google.mlkit.vision.barcode.common.Barcode
import com.google.mlkit.vision.codescanner.GmsBarcodeScannerOptions
import com.google.mlkit.vision.codescanner.GmsBarcodeScanning

/**
 * Play flavor: Google's code scanner (its own UI, no camera permission for us). Its UI is
 * a module Play Services downloads on first use, so [prepare] asks for it at app start;
 * a scan that can't start yet says so instead of doing nothing.
 */
object QrScanner {
    private fun client(context: Context) =
        GmsBarcodeScanning.getClient(context, GmsBarcodeScannerOptions.Builder().setBarcodeFormats(Barcode.FORMAT_QR_CODE).build())

    /** Download the scanner module in the background, if Play Services lacks it. */
    fun prepare(context: Context) {
        runCatching {
            ModuleInstall.getClient(context).installModules(ModuleInstallRequest.newBuilder().addApi(client(context)).build())
        }
    }

    fun scan(activity: ComponentActivity, onResult: (String) -> Unit) {
        runCatching {
            client(activity).startScan()
                .addOnSuccessListener { code -> code.rawValue?.let(onResult) }
                .addOnFailureListener {
                    // Placeholder wording (UX pending): usually the scanner is still downloading.
                    Toast.makeText(activity, "The scanner isn't ready yet. Try again in a moment, or type the code.", Toast.LENGTH_LONG).show()
                    prepare(activity)
                }
        }.onFailure {
            Toast.makeText(activity, "Scanning isn't available on this phone. Type the code instead.", Toast.LENGTH_LONG).show()
        }
    }
}
