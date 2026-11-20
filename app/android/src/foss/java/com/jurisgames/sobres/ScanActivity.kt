package com.jurisgames.sobres

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.core.content.ContextCompat
import java.util.concurrent.Executors

/** foss flavor: camera preview; returns the first QR code it reads (camera asked only here). */
class ScanActivity : ComponentActivity() {
    private val analysis = Executors.newSingleThreadExecutor()
    private val askCamera = registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) start() else finish()
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        if (checkSelfPermission(Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) start() else askCamera.launch(Manifest.permission.CAMERA)
    }

    private fun start() {
        val view = PreviewView(this)
        setContentView(view)
        val future = ProcessCameraProvider.getInstance(this)
        future.addListener({
            val provider = future.get()
            val preview = Preview.Builder().build().also { it.surfaceProvider = view.surfaceProvider }
            val reader = ImageAnalysis.Builder().setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST).build()
            reader.setAnalyzer(analysis) { frame ->
                val plane = frame.planes[0]
                val bytes = ByteArray(plane.buffer.remaining()).also { plane.buffer.get(it) }
                val text = QrDecode.fromLuminance(bytes, plane.rowStride, frame.width, frame.height)
                frame.close()
                if (text != null) runOnUiThread {
                    setResult(RESULT_OK, Intent().putExtra("text", text))
                    finish()
                }
            }
            provider.unbindAll()
            provider.bindToLifecycle(this, CameraSelector.DEFAULT_BACK_CAMERA, preview, reader)
        }, ContextCompat.getMainExecutor(this))
    }

    override fun onDestroy() {
        analysis.shutdown()
        super.onDestroy()
    }
}
