package com.rp.chat

import android.app.Activity
import android.content.Intent
import android.os.SystemClock
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

    // Generation counter that increments on every Activity recreation (each
    // MainActivity.onCreate -> setLauncher call). Together with pendingGeneration
    // it lets us distinguish:
    //   - a genuine concurrent double-tap within the SAME Activity instance
    //     (pendingGeneration == generation => reject as "already picking")
    //   - a stale orphaned pick from a PREVIOUS Activity instance that never got
    //     its result (pendingGeneration != generation => safe to clear and allow
    //     the new pick).
    //
    // Why this exists (issue 1 - "already picking" stuck):
    // Before this fix pendingInvoke was only cleared in onActivityResult and on
    // launch exception. If the Activity was destroyed/recreated mid-pick (e.g.
    // swipe-away + relaunch, or config change while the system picker is on top)
    // the result is never delivered: the old launcher is unregistered/destroyed
    // and the new Activity's launcher has no pending state. pendingInvoke then
    // stays set forever and every future tap is rejected as "already picking"
    // even though no picker is showing. The generation token + setLauncher
    // clearing below ensures a stale guard cannot permanently block picking,
    // while a true concurrent double-invoke in the same Activity instance is
    // still rejected.
    @Volatile
    private var generation: Int = 0

    @Volatile
    private var pendingGeneration: Int = -1

    // `elapsedRealtime` stamp of the in-flight pick. The generation token alone
    // cannot express "orphaned but no recreation" — setLauncher increments the
    // generation AND clears pendingInvoke, so `pendingGeneration != generation`
    // is unreachable while pendingInvoke is set. This timestamp gives the guard
    // an independent liveness signal so a same-instance lost result cannot wedge
    // the picker forever.
    @Volatile
    private var pendingSince: Long = 0L

    // A pending pick older than this is treated as orphaned. Comfortably longer
    // than any real picker interaction, so it never races a live pick.
    private const val PICK_STALE_MS = 120_000L

    /**
     * Point the picker at the current activity's launcher. Called from
     * `MainActivity.onCreate`, so it re-binds to THIS activity on every
     * creation — which is what keeps the launcher valid across recreation.
     * Main thread only.
     *
     * FIX for stuck picker (issue 1):
     * If a pick was in-flight when the Activity was destroyed, pendingInvoke
     * would otherwise remain set forever (result never arrives on the old
     * launcher). We treat any pending pick whose generation != new generation
     * as orphaned: resolve it as cancelled (empty result -> Rust gets empty vec)
     * and clear the guard so the next user tap can launch a fresh picker.
     * This is eager clearing on Activity recreation; pickFiles also has a lazy
     * clear as a safety net if setLauncher was not called (e.g. same Activity
     * but result lost for other reasons).
     *
     * We resolve with the no-arg `invoke.resolve()` (literal null -> None on
     * Rust side = cancelled) rather than leaving the JS promise hanging, so the
     * frontend's await unblocks and can retry. If the old invoke's JS context
     * is already dead (WebView recreated), resolving is a no-op but still frees
     * the guard.
     */
    @JvmStatic
    fun setLauncher(l: (Intent) -> Unit) {
      generation += 1
      val stale = pendingInvoke
      if (stale != null) {
        // Orphaned pick from a previous Activity instance (generation mismatch).
        // Clear it now; otherwise the next pick would be incorrectly rejected
        // as "already picking" and the UI would be stuck.
        Log.w(TAG, "setLauncher: clearing orphaned pick (gen=$pendingGeneration -> $generation)")
        try {
          // Resolve as cancelled so Rust's Option<Vec<String>> gets None -> empty vec
          stale.resolve()
        } catch (e: Exception) {
          Log.w(TAG, "setLauncher: failed to resolve orphaned invoke", e)
        }
        pendingInvoke = null
        pendingGeneration = -1
        pendingSince = 0L
      }
      launcher = l
      Log.d(TAG, "setLauncher: generation=$generation")
    }

    /**
     * Entry point from `MainActivity`'s `registerForActivityResult` callback.
     * Main thread only.
     *
     * Note: we intentionally do NOT check generation here. If the Activity was
     * recreated while the picker was showing, the system may forward the result
     * to the new Activity's launcher (via Activity recreation handling). In that
     * case pendingGeneration (old) != generation (new) but the result still
     * belongs to the orphaned pick and should be delivered. Since we already
     * cleared orphaned picks eagerly in setLauncher, this path only runs for
     * the normal case where the same Activity instance launched and received
     * the result. If a forwarded result does arrive after eager clearing,
     * pendingInvoke will be null and we return early (harmless).
     */
    @JvmStatic
    fun onActivityResult(resultCode: Int, data: Intent?) {
      val invoke = pendingInvoke ?: return
      pendingInvoke = null
      pendingGeneration = -1
      pendingSince = 0L

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
    val pending = pendingInvoke
    if (pending != null) {
      val now = SystemClock.elapsedRealtime()
      val stale = pendingGeneration != generation || now - pendingSince > PICK_STALE_MS
      if (!stale) {
        // Genuine concurrent double-invoke within the same Activity instance:
        // the previous pick is still in-flight and its launcher/system picker
        // is on top. Reject so we don't launch a second picker on top.
        invoke.reject("picker: already picking")
        return
      }
      // Orphaned pick: either a recreation setLauncher missed, or the result was
      // never delivered within the same Activity instance (no generation change,
      // hence the age check). Clear it and fall through to launch a fresh pick,
      // so this guard can never wedge permanently.
      Log.w(
        TAG,
        "pickFiles: clearing stale pending (gen=$pendingGeneration vs $generation, age=${now - pendingSince}ms)",
      )
      try {
        pending.resolve()
      } catch (e: Exception) {
        Log.w(TAG, "pickFiles: failed to resolve stale invoke", e)
      }
      pendingInvoke = null
      pendingGeneration = -1
      pendingSince = 0L
    }

    val intent = Intent(Intent.ACTION_OPEN_DOCUMENT)
    intent.addCategory(Intent.CATEGORY_OPENABLE)
    intent.type = "*/*"
    intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)

    pendingInvoke = invoke
    pendingGeneration = generation
    pendingSince = SystemClock.elapsedRealtime()
    try {
      launch(intent)
    } catch (e: Exception) {
      // Launch itself failed (e.g. no documents provider): un-park so the next
      // pick is not rejected as "already picking".
      pendingInvoke = null
      pendingGeneration = -1
      Log.w(TAG, "pickFiles: launch failed", e)
      invoke.reject("picker: ${e.message}")
    }
  }
}
