package com.rp.chat

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.Settings
import android.util.Log
import app.tauri.annotation.Command
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin

/**
 * All-files shared-storage access (`MANAGE_EXTERNAL_STORAGE`).
 *
 * Host tools work on the user's real files under `/storage/emulated/0`, and on
 * Android 11+ broad shared-storage access requires MANAGE_EXTERNAL_STORAGE. That
 * permission is special: it is **not** a runtime prompt — the user grants it
 * from the system "All files access" Settings screen. This plugin therefore only
 * exposes two things to Rust (`crate::android::storage_status` /
 * `request_storage_access`):
 *
 *  - `status`: is it granted, and can this OS grant it at all (API >= 30)?
 *  - `requestAccess`: open the app-specific Settings screen (falling back to the
 *    global all-files list when an OEM build does not resolve the app screen).
 *
 * The frontend decides when to call `requestAccess` (Settings → Storage); there
 * is deliberately no startup nag. Rust reaches this class through
 * `crate::android::storage_plugin` under the plugin name `storage_access`, and
 * both Tauri commands are async so the plugin bridge never blocks the main
 * thread (the deadlock the notification commands hit).
 */
@TauriPlugin
class StorageAccessPlugin(private val activity: Activity) : Plugin(activity) {

  companion object {
    private const val TAG = "StorageAccessPlugin"
  }

  /** Whether the app can currently read/write shared storage broadly. */
  private fun hasAllFilesAccess(): Boolean =
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
      Environment.isExternalStorageManager()
    } else {
      // Pre-11 devices have no MANAGE_EXTERNAL_STORAGE; the legacy
      // READ/WRITE_EXTERNAL_STORAGE permissions from the manifest apply.
      true
    }

  /** Report `{ granted, canRequest }` to Rust. */
  @Command
  fun status(invoke: Invoke) {
    val ret = JSObject()
    ret.put("granted", hasAllFilesAccess())
    // canRequest is true only on API 30+, where the all-files Settings screen
    // (and the permission itself) exists. The frozen frontend contract expects
    // exactly this.
    ret.put("canRequest", Build.VERSION.SDK_INT >= Build.VERSION_CODES.R)
    invoke.resolve(ret)
  }

  /**
   * Open the system screen where the user grants all-files access. Resolves as
   * soon as the screen is launched: access is granted in Settings, and the
   * frontend re-checks [status] when the app returns to the foreground.
   */
  @Command
  fun requestAccess(invoke: Invoke) {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) {
      invoke.reject("all-files access settings require Android 11 (API 30) or newer")
      return
    }
    if (hasAllFilesAccess()) {
      // Already granted: nothing to open (the status re-check will confirm).
      invoke.resolve()
      return
    }

    val appIntent = Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION).apply {
      data = Uri.parse("package:${activity.packageName}")
    }
    val allIntent = Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION)
    val pkg = activity.packageName

    // Launch from the UI thread: starting an Activity from the plugin bridge
    // thread works on most builds but is not guaranteed by the platform.
    activity.runOnUiThread {
      try {
        activity.startActivity(appIntent)
        invoke.resolve()
      } catch (e: Exception) {
        // Some OEM builds do not resolve the per-app screen; fall back to the
        // global all-files list, where the user can pick Pi Chat.
        Log.w(TAG, "requestAccess: per-app settings screen unavailable ($pkg), falling back", e)
        try {
          activity.startActivity(allIntent)
          invoke.resolve()
        } catch (inner: Exception) {
          Log.w(TAG, "requestAccess: no all-files settings screen available", inner)
          invoke.reject("could not open all-files access settings: ${inner.message}")
        }
      }
    }
  }
}
