package com.luckymatch.app.audio

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.media.projection.MediaProjection
import android.media.projection.MediaProjectionManager
import android.os.Build
import android.os.IBinder
import androidx.core.app.NotificationCompat

class MediaProjectionService : Service() {

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_START -> {
                ensureNotificationChannel()
                val notification = buildNotification()
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                    startForeground(
                        NOTIFICATION_ID,
                        notification,
                        ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION
                    )
                } else {
                    startForeground(NOTIFICATION_ID, notification)
                }

                val resultCode = intent.getIntExtra(EXTRA_RESULT_CODE, 0)
                val resultData: Intent? = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                    intent.getParcelableExtra(EXTRA_RESULT_DATA, Intent::class.java)
                } else {
                    @Suppress("DEPRECATION")
                    intent.getParcelableExtra(EXTRA_RESULT_DATA)
                }

                if (resultCode != 0 && resultData != null) {
                    val mgr = getSystemService(Context.MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
                    activeProjection?.stop()
                    activeProjection = mgr.getMediaProjection(resultCode, resultData)
                    onProjectionReady?.invoke(activeProjection != null)
                } else {
                    onProjectionReady?.invoke(false)
                }
            }
            ACTION_STOP -> {
                releaseProjection()
                stopForeground(STOP_FOREGROUND_REMOVE)
                stopSelf()
            }
        }
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        releaseProjection()
        super.onDestroy()
    }

    private fun ensureNotificationChannel() {
        val mgr = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        val channel = NotificationChannel(
            CHANNEL_ID,
            "Internal Audio Capture",
            NotificationManager.IMPORTANCE_LOW
        ).apply {
            description = "Active while LuckyMatch captures device audio for song recognition"
        }
        mgr.createNotificationChannel(channel)
    }

    private fun buildNotification(): Notification {
        return NotificationCompat.Builder(this, CHANNEL_ID)
            .setContentTitle("LuckyMatch")
            .setContentText("Capturing device audio for live lyrics sync")
            .setSmallIcon(android.R.drawable.ic_btn_speak_now)
            .setOngoing(true)
            .setSilent(true)
            .build()
    }

    companion object {
        private const val CHANNEL_ID = "luckymatch_capture"
        private const val NOTIFICATION_ID = 1001
        const val ACTION_START = "com.luckymatch.app.action.START_PROJECTION"
        const val ACTION_STOP = "com.luckymatch.app.action.STOP_PROJECTION"
        const val EXTRA_RESULT_CODE = "extra_result_code"
        const val EXTRA_RESULT_DATA = "extra_result_data"

        @Volatile
        var activeProjection: MediaProjection? = null
            private set

        @Volatile
        var onProjectionReady: ((Boolean) -> Unit)? = null

        fun releaseProjection() {
            activeProjection?.stop()
            activeProjection = null
        }
    }
}
