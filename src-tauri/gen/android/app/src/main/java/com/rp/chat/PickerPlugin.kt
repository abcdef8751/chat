package com.rp.chat

import android.app.Activity
import android.content.Intent
import android.util.Log
import app.tauri.annotation.Command
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.Plugin
import java.util.ArrayList

/**
 * The app's own Android document picker, replacing `tauri-plugin-dialog` there.
 *
 * Why: Tauri's `PluginManager.onActivityCreate` early-returns after the first
 * run, so the picker's `ActivityResultLauncher` it registers stays bound to the
 * *destroyed* Activity. This app recreates its Activity constantly (the process
 * is kept alive by `prevent_exit()` + `KeepAliveService` after a swipe-away, and
 * relaunching from the icon builds a fresh Activity), after which every
 * `showFilePicker` fails with "Attempting to launch an unregistered
 * ActivityResultLauncher" — the picker silently stops working until the process
 * dies. (Upstream tauri-apps/tauri#15506; fixed in 2.12.x, which we cannot take
 * because of the vendored `tauri-runtime-wry` patch.)
 *
 * The fix: register the launcher in [MainActivity]'s field initializer (the one
 * place registration is legal, and it re-runs on every Activity creation) and
 * hand it here via [setLauncher]. Rust reaches this class through
 * `crate::android::picker_plugin` under the plugin name `picker`; the JS side
 * calls the `pick_attachment_uris` command.
 *
 * The picked `content://` URIs go back to Rust unchanged, where they are read by
 * the existing content-URI bridge (`FileBridgePlugin.readUri` /
 * `read_attachment_from_uri`).
 */
@TauriPlugin
class PickerPlugin(private val activity: Activity) : Plugin(activity) {

  companion object {
    private const val TAG = "PickerPlugin"

    // The launcher registered by `MainActivity.onCreate`. `null` until the
    // activity has been created (i.e. before the webview can invoke anything).
    @Volatile
    private var launcher: ((Intent) -> Unit)? = null

    // The invoke of an in-flight pick. It lives on the (static) companion on
    // purpose: the activity that started the pick can be destroyed and recreated
    // before the result arrives, and [onActivityResult] is a static entry point
    // so the result still finds it.
    @Volatile
    private var pendingInvoke: Invoke? = null

    /**
     * Point the picker at the current activity's launcher. Called from
     * `MainActivity.onCreate`, so it re-binds to THIS activity on every
     * creation — which is what keeps the launcher valid across recreation.
     * Main thread only.
     */
    @JvmStatic
    fun setLauncher(l: (Intent) -> Unit) {
      launcher = l
    }

    /**
     * Entry point from `MainActivity`'s `registerForActivityResult` callback.
     * Main thread only.
     */
    @JvmStatic
    fun onActivityResult(resultCode: Int, data: Intent?) {
      val invoke = pendingInvoke ?: return
      pendingInvoke = null

      if (resultCode != Activity.RESULT_OK || data == null) {
        // Cancelled (back press / dismissed). NOTE: `resolve(null)` would bind
        // to `resolve(data: JSObject?)`, whose `PluginResult(null)` serializes
        // as `{}` — which Rust's `Option<Vec<String>>` cannot deserialize. The
        // no-arg overload sends the literal `null`, which deserializes to
        // `None` → an empty (cancelled) result on the Rust side.
        invoke.resolve()
        return
      }

      val uris = ArrayList<String>()
      val clip = data.clipData
      if (clip != null) {
        for (i in 0 until clip.itemCount) {
          uris.add(clip.getItemAt(i).uri.toString())
        }
      } else {
        data.data?.let { uris.add(it.toString()) }
      }
      // `resolveObject` with a plain `List<String>`: Jackson serializes it as a
      // top-level JSON array, which is what Rust deserializes into
      // `Option<Vec<String>>`. (`JSArray` cannot be used here: it extends
      // org.json.JSONArray, and Jackson has no org.json module on this
      // classpath, so it is not seen as a collection — it would serialize as an
      // object, or fail outright with "No serializer found".)
      invoke.resolveObject(uris)
    }
  }

  /** Launch the system document picker; resolve with the chosen URIs. */
  @Command
  fun pickFiles(invoke: Invoke) {
    val launch = launcher
    if (launch == null) {
      invoke.reject("picker: launcher not ready")
      return
    }
    if (pendingInvoke != null) {
      invoke.reject("picker: already picking")
      return
    }

    val intent = Intent(Intent.ACTION_OPEN_DOCUMENT)
    intent.addCategory(Intent.CATEGORY_OPENABLE)
    intent.type = "*/*"
    intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)

    pendingInvoke = invoke
    try {
      launch(intent)
    } catch (e: Exception) {
      // Launch itself failed (e.g. no documents provider): un-park so the next
      // pick is not rejected as "already picking".
      pendingInvoke = null
      Log.w(TAG, "pickFiles: launch failed", e)
      invoke.reject("picker: ${e.message}")
    }
  }
}
