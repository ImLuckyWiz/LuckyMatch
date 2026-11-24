package com.luckymatch.app

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Color
import android.media.projection.MediaProjectionManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.provider.Settings
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
import androidx.core.content.FileProvider
import com.luckymatch.app.audio.CaptureSource
import com.luckymatch.app.audio.MediaProjectionService
import com.luckymatch.app.ui.KaraokeScreen
import com.luckymatch.app.ui.LuckyMatchViewModel
import java.io.File

class MainActivity : ComponentActivity() {

    private val viewModel: LuckyMatchViewModel by viewModels()
    private var pendingIdentifyAfterPermission = false
    private var pendingInstallApk: File? = null

    private val unknownSourcesLauncher = registerForActivityResult(
        ActivityResultContracts.StartActivityForResult()
    ) {
        val apk = pendingInstallApk
        if (apk != null && packageManager.canRequestPackageInstalls()) {
            pendingInstallApk = null
            launchApkInstaller(apk)
        }
    }

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
                    onInstallApk = ::installApk,
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

    private fun installApk(apkFile: File) {
        if (!apkFile.exists()) return
        if (!packageManager.canRequestPackageInstalls()) {
            pendingInstallApk = apkFile
            val intent = Intent(
                Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES,
                Uri.parse("package:$packageName")
            )
            unknownSourcesLauncher.launch(intent)
            return
        }
        launchApkInstaller(apkFile)
    }

    private fun launchApkInstaller(apkFile: File) {
        val uri = FileProvider.getUriForFile(this, "$packageName.fileprovider", apkFile)
        val intent = Intent(Intent.ACTION_VIEW).apply {
            setDataAndType(uri, "application/vnd.android.package-archive")
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        startActivity(intent)
    }

    override fun onDestroy() {
        if (isFinishing) {
            MediaProjectionService.releaseProjection()
            stopService(Intent(this, MediaProjectionService::class.java))
        }
        super.onDestroy()
    }
}
