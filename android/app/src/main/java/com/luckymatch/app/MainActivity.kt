package com.luckymatch.app

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Color
import android.media.projection.MediaProjectionManager
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.SystemBarStyle
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.activity.viewModels
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.ui.graphics.Color as ComposeColor
import androidx.core.content.ContextCompat
import com.luckymatch.app.audio.CaptureSource
import com.luckymatch.app.audio.MediaProjectionService
import com.luckymatch.app.ui.KaraokeScreen
import com.luckymatch.app.ui.LuckyMatchViewModel

class MainActivity : ComponentActivity() {

    private val viewModel: LuckyMatchViewModel by viewModels()
    private var pendingIdentifyAfterPermission = false

    private val requestMicPermissionLauncher = registerForActivityResult(
        ActivityResultContracts.RequestPermission()
    ) { granted ->
        if (granted) {
            viewModel.setSource(CaptureSource.MIC)
            if (pendingIdentifyAfterPermission) {
                pendingIdentifyAfterPermission = false
                viewModel.identifyNow()
            }
        } else {
            pendingIdentifyAfterPermission = false
        }
    }

    private val requestProjectionLauncher = registerForActivityResult(
        ActivityResultContracts.StartActivityForResult()
    ) { result ->
        if (result.resultCode == RESULT_OK && result.data != null) {
            MediaProjectionService.onProjectionReady = { ready ->
                if (ready) {
                    viewModel.setSource(CaptureSource.INTERNAL_AUDIO)
                    if (pendingIdentifyAfterPermission) {
                        pendingIdentifyAfterPermission = false
                        viewModel.identifyNow()
                    }
                } else {
                    pendingIdentifyAfterPermission = false
                }
            }
            val svcIntent = Intent(this, MediaProjectionService::class.java).apply {
                action = MediaProjectionService.ACTION_START
                putExtra(MediaProjectionService.EXTRA_RESULT_CODE, result.resultCode)
                putExtra(MediaProjectionService.EXTRA_RESULT_DATA, result.data)
            }
            ContextCompat.startForegroundService(this, svcIntent)
        } else {
            pendingIdentifyAfterPermission = false
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge(
            statusBarStyle = SystemBarStyle.dark(Color.TRANSPARENT),
            navigationBarStyle = SystemBarStyle.dark(Color.TRANSPARENT),
        )

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            if (ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS)
                != PackageManager.PERMISSION_GRANTED
            ) {
                requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 2001)
            }
        }

        setContent {
            MaterialTheme(
                colorScheme = darkColorScheme(
                    primary = ComposeColor(0xFF38BDF8),
                    background = ComposeColor(0xFF080A0F),
                    surface = ComposeColor(0xFF141824),
                    onBackground = ComposeColor.White,
                    onSurface = ComposeColor.White,
                )
            ) {
                KaraokeScreen(
                    viewModel = viewModel,
                    onRequestIdentify = { source ->
                        ensureSourceAndRun(source, startIdentify = true)
                    },
                    onRequestSwitchSource = { nextSource ->
                        ensureSourceAndRun(nextSource, startIdentify = false)
                    },
                )
            }
        }
    }

    private fun ensureSourceAndRun(source: CaptureSource, startIdentify: Boolean) {
        when (source) {
            CaptureSource.MIC -> {
                val granted = ContextCompat.checkSelfPermission(
                    this,
                    Manifest.permission.RECORD_AUDIO
                ) == PackageManager.PERMISSION_GRANTED

                if (granted) {
                    viewModel.setSource(CaptureSource.MIC)
                    if (startIdentify) viewModel.identifyNow()
                } else {
                    pendingIdentifyAfterPermission = startIdentify
                    requestMicPermissionLauncher.launch(Manifest.permission.RECORD_AUDIO)
                }
            }
            CaptureSource.INTERNAL_AUDIO -> {
                if (MediaProjectionService.activeProjection != null) {
                    viewModel.setSource(CaptureSource.INTERNAL_AUDIO)
                    if (startIdentify) viewModel.identifyNow()
                } else {
                    pendingIdentifyAfterPermission = startIdentify
                    val mgr = getSystemService(MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
                    requestProjectionLauncher.launch(mgr.createScreenCaptureIntent())
                }
            }
        }
    }

    override fun onDestroy() {
        if (isFinishing) {
            MediaProjectionService.releaseProjection()
            stopService(Intent(this, MediaProjectionService::class.java))
        }
        super.onDestroy()
    }
}
