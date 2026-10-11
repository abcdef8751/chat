package com.rp.chat

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import android.util.Log

/**
 * Keeps the app's process — and therefore its Rust background schedulers (cloud
 * sync + idle memory reflection, both ~60s ticks) — alive after the user swipes
 * the app away from recents.
 *
 * Why this exists: when the last activity/window is destroyed, Tauri calls
 * `std::process::exit(0)` (see the matching `prevent_exit()` in lib.rs), which
 * would kill the process and all its tokio tasks. This foreground service pins
 * the process so Android doesn't reclaim it either. The persistent notification
 * is the unavoidable cost of any background execution on modern Android.
 *
 * It carries no logic of its own; the work it keeps alive lives in Rust
 * (`reflection::spawn` / `sync::spawn`). `START_STICKY` lets the OS restart it
 * after a low-memory kill, and `onTaskRemoved` deliberately keeps it running so
 * swiping the app away does not stop the schedulers.
 */
class KeepAliveService : Service() {

    companion object {
        private const val TAG = "KeepAliveService"
        private const val CHANNEL_ID = "keep_alive"
        private const val NOTIF_ID = 1
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        startAsForeground()
        return START_STICKY
    }

    private fun startAsForeground() {
        val nm = getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(
            NotificationChannel(CHANNEL_ID, "Background sync", NotificationManager.IMPORTANCE_LOW)
        )
        val openApp = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )
        val notif = Notification.Builder(this, CHANNEL_ID)
            .setContentTitle("Pi Chat is running")
            .setContentText("Cloud sync and memory stay up to date.")
            .setContentIntent(openApp)
            .setOngoing(true)
            .setSmallIcon(android.R.drawable.ic_menu_info_details)
            .build()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(
                NOTIF_ID,
                notif,
                ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC
            )
        } else {
            startForeground(NOTIF_ID, notif)
        }
    }

    /** User swiped the app away from recents — keep running as a foreground service. */
    override fun onTaskRemoved(rootIntent: Intent?) {
        super.onTaskRemoved(rootIntent)
        Log.i(TAG, "task removed; continuing as foreground service")
    }

    /**
     * Android 15+ (API 35) caps dataSync foreground services at 6h per 24h of
     * background time. Stop cleanly instead of crashing; the schedulers resume
     * next time the user brings the app forward (which resets the timer).
     */
    override fun onTimeout(startId: Int) {
        Log.i(TAG, "dataSync foreground-service timeout; stopping cleanly")
        stopSelf()
    }
}
