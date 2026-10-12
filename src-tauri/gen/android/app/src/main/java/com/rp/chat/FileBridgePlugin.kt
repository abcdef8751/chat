package com.rp.chat

import android.app.Activity
import android.net.Uri
import android.provider.OpenableColumns
import android.util.Base64
import android.util.Log
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.ByteArrayOutputStream

/**
 * Resolves an Android `content://` URI to bytes + mime + display name.
 *
 * `tauri-plugin-dialog` returns `content://` URIs on Android, which `std::fs`
 * cannot open, so the path-based reader used to drop every picked file. This
 * plugin reads the URI through the `ContentResolver` instead and hands the
 * data to Rust (`crate::android::read_attachment_from_uri`), which builds the
 * same `Attachment` shape as the desktop path reader. See `src/lib/platform.ts`
 * (`readAttachmentFromUri`) for the JS-side wrapper.
 *
 * Registered from `crate::android::file_plugin` and invoked over the mobile
 * plugin bridge (`run_mobile_plugin_async`), so it is Android-only.
 */
@TauriPlugin
class FileBridgePlugin(private val activity: Activity) : Plugin(activity) {

  companion object {
    private const val TAG = "FileBridgePlugin"

    // Upper bound on what we pull into memory. Images and text are the target;
    // an oversized file is rejected rather than risking an OOM.
    private const val MAX_BYTES = 25 * 1024 * 1024
  }

  @Command
  fun readUri(invoke: Invoke) {
    val args = invoke.parseArgs(ReadUriArgs::class.java)
    val uriString = args.uri
    if (uriString.isNullOrBlank()) {
      invoke.reject("readUri: missing uri")
      return
    }

    try {
      val uri = Uri.parse(uriString)
      val resolver = activity.contentResolver
      val name = displayName(uri)
      val mime = resolver.getType(uri) ?: ""

      // Read at most MAX_BYTES + one chunk so an oversized file is detected
      // without first buffering the whole thing.
      val bytes = resolver.openInputStream(uri)?.use { input ->
        val buf = ByteArrayOutputStream()
        val chunk = ByteArray(64 * 1024)
        var total = 0
        while (total <= MAX_BYTES) {
          val n = input.read(chunk)
          if (n < 0) break
          total += n
          buf.write(chunk, 0, n)
        }
        buf.toByteArray()
      }
      if (bytes == null) {
        invoke.reject("readUri: cannot open $uriString")
        return
      }
      if (bytes.size > MAX_BYTES) {
        invoke.reject("readUri: file too large (> $MAX_BYTES bytes)")
        return
      }

      val ret = JSObject()
      ret.put("name", name)
      ret.put("mime", mime)
      ret.put("size", bytes.size)
      // Standard base64, no wrapping — Rust's base64 crate decodes it as-is.
      ret.put("data", Base64.encodeToString(bytes, Base64.NO_WRAP))
      invoke.resolve(ret)
    } catch (e: Exception) {
      Log.w(TAG, "readUri($uriString) failed", e)
      invoke.reject("readUri: ${e.message}")
    }
  }

  /**
   * Query `OpenableColumns.DISPLAY_NAME`, falling back to the URI's last path
   * segment (document providers must implement DISPLAY_NAME, but not every
   * `content://` URI does).
   */
  private fun displayName(uri: Uri): String {
    var name: String? = null
    try {
      activity.contentResolver
        .query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)
        ?.use { cursor ->
          val idx = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
          if (idx >= 0 && cursor.moveToFirst()) {
            name = cursor.getString(idx)
          }
        }
    } catch (e: Exception) {
      Log.w(TAG, "displayName($uri) failed", e)
    }
    return name?.takeIf { it.isNotEmpty() } ?: uri.lastPathSegment ?: "attachment"
  }
}

/**
 * Args for `FileBridgePlugin.readUri`. A plain POJO with a no-arg constructor
 * and mutable properties, NOT a Kotlin `data class`: Tauri deserializes the
 * JSON payload into this with Jackson, which needs the no-arg constructor +
 * setters. `@InvokeArg` is what Tauri's consumer ProGuard rules use to keep the
 * class (and its members) from being stripped/renamed in release builds.
 */
@InvokeArg
class ReadUriArgs {
  var uri: String? = null
}
