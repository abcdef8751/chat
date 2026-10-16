package com.rp.chat

import android.annotation.SuppressLint
import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.util.Log
import android.view.WindowInsets
import android.webkit.JavascriptInterface
import android.webkit.WebView
import androidx.activity.OnBackPressedCallback
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts

class MainActivity : TauriActivity() {
  // Latest system-bar insets (status bar + nav bar + display cutout), in **dp**
  // (CSS px). Kept as volatile fields so the JS bridge can read them
  // synchronously at any time.
  @Volatile
  private var topInset = 0
  @Volatile
  private var bottomInset = 0
  @Volatile
  private var leftInset = 0
  @Volatile
  private var rightInset = 0

  // ---- IME language-switcher fix (issue 7) state ----
  // When the IME's language/long-press selector is shown, some keyboards
  // briefly report isVisible(IME)==false while the keyboard is still
  // logically present. Our old applyInsets immediately shrank bottomInset
  // to the nav-bar height, causing the web flex layout to relayout and the
  // focused input to lose focus, which dismisses the keyboard entirely so
  // the language can't be changed.
  //
  // We track visibility transitions and ignore a single transient "hidden"
  // after a visible state if the previous bottomInset was keyboard-sized.
  // A real dismiss will produce at least 2 consecutive hidden dispatches,
  // so the second one collapses the inset as expected. This is a best-effort
  // heuristic; device verification is required because IME behavior varies by
  // keyboard app and Android version.
  private var wasImeVisible = false
  private var consecutiveImeHidden = 0

  // The live WebView, captured in onWebViewCreate. Used to dispatch the
  // `pi-back` event when the hardware/gesture back is pressed.
  @Volatile
  private var appWebView: WebView? = null

  // Our own document picker (see PickerPlugin.kt). Registered here — a field
  // initializer is the documented, legal registration point (before STARTED) —
  // because `tauri-plugin-dialog`'s own launcher stays bound to the *destroyed*
  // Activity after an Activity recreation, which happens on every relaunch here.
  // `onCreate` re-points the plugin at this launcher on each creation.
  private val pickerLauncher =
    registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
      PickerPlugin.onActivityResult(result.resultCode, result.data)
    }

  // Convert physical px (what getInsets/setPadding use) to dp (what CSS px
  // means), so the values this app injects as `px` in its styles scale right.
  private fun dp(px: Int): Int = Math.round(px / resources.displayMetrics.density).toInt()

  // JS-visible object (`window.PiInsets`). The web app reads the current
  // insets on mount and subscribes to the `pi-insets` event for live updates,
  // then reserves the space itself — this is deterministic and does not depend
  // on Android WebView's flaky padding/fitsSystemWindows behavior. Values are
  // returned in dp (CSS px): `getInsets()` yields physical px, which the web
  // layer would treat as CSS px and over-inset by the display density.
  private inner class InsetsBridge {
    @JavascriptInterface
    fun getTop(): Int = topInset
    @JavascriptInterface
    fun getBottom(): Int = bottomInset
    @JavascriptInterface
    fun getLeft(): Int = leftInset
    @JavascriptInterface
    fun getRight(): Int = rightInset
  }

  // JS-visible object (`window.PiBack`) — the JS→native half of the back
  // bridge. The web layer calls `exit()` when it has exhausted its own history
  // (nothing left to pop) and wants the activity to finish. Hardware back does
  // NOT need this: `handleBack` finishes on its own when `pi-back` is
  // unhandled. Exposed so an in-page "Quit"/"Exit" affordance can leave the app
  // deliberately instead of only via the system back gesture.
  private inner class BackBridge {
    @JavascriptInterface
    fun exit() {
      // JavascriptInterface methods run on a WebView bridge thread, so bounce
      // to the UI thread before touching the Activity.
      runOnUiThread { finish() }
    }
  }

  override fun onCreate(savedInstanceState: Bundle?) {
    // Edge-to-edge is enforced on Android 15+ (targetSdk 35+); the app draws
    // behind the status/nav bars. We translate the bars into insets exposed to
    // the web layer rather than relying on WebView padding (see onWebViewCreate).
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    // Hand this activity's picker launcher to the static plugin. Doing it here
    // (rather than once in the plugin) is the whole fix: `onCreate` runs again
    // for every recreated Activity, so the launcher is always bound to the live
    // one — `tauri-plugin-dialog` keeps its own launcher bound to the first
    // Activity and fails after a recreation.
    PickerPlugin.setLauncher { intent -> pickerLauncher.launch(intent) }
    // Register Android's system trust store with rustls-platform-verifier
    // (called on the main thread, single time). Without this, reqwest 0.13's
    // first HTTPS request SIGABRTs the process:
    // "Expect rustls-platform-verifier to be initialized".
    initRustls()
    // Keep the app (and its Rust sync/reflection schedulers) alive after the user
    // swipes it away. Started from onCreate so we're reliably in the foreground
    // (Android 12+ forbids starting foreground services from the background),
    // then START_STICKY keeps it running; best-effort, wrapped in a guard anyway.
    try {
      startForegroundService(Intent(this, KeepAliveService::class.java))
    } catch (e: Exception) {
      Log.w("MainActivity", "could not start keep-alive service", e)
    }
    // Hardware/gesture back: route it through the web layer instead of always
    // finishing. TauriActivity sets `handleBackNavigation = false`, so without
    // this the system back would simply destroy the activity on every page.
    // The callback (enabled = true) disables the default finish; `handleBack`
    // finishes explicitly only when the page reports it did not handle the
    // press. This fires for both the 3-button/gesture back and (when the OS
    // routes it) predictive back.
    onBackPressedDispatcher.addCallback(
      this,
      object : OnBackPressedCallback(true) {
        override fun handleOnBackPressed() {
          handleBack()
        }
      }
    )
  }

  /**
   * Dispatch a cancelable `pi-back` event into the page (the JS half lives in
   * `src/lib/platform.ts`). A listener that consumed the press calls
   * `preventDefault()`; if nobody did, there is nothing left to pop, so the
   * activity finishes. `evaluateJavascript` hands the JS result to the
   * callback as JSON — a boolean arrives as the literal `"true"`/`"false"`.
   */
  private fun handleBack() {
    val webView = appWebView
    if (webView == null) {
      finish()
      return
    }
    webView.evaluateJavascript(BACK_JS) { result ->
      if (result != "true") {
        finish()
      }
    }
  }

  // The Termux RUN_COMMAND permission is a runtime grant owned by Termux; our
  // RunCommandPlugin requests it and parks the pending command. Route the result
  // back so it can resume (or report a denial) instead of silently hanging.
  override fun onRequestPermissionsResult(
    requestCode: Int,
    permissions: Array<out String>,
    grantResults: IntArray,
  ) {
    super.onRequestPermissionsResult(requestCode, permissions, grantResults)
    RunCommandPlugin.onPermissionResult(requestCode, grantResults)
  }

  // WryActivity calls this once the WebView is created. We expose the system-bar
  // insets to the web content via `window.PiInsets` + a `pi-insets` event, and let
  // the app's own flex layout reserve the space (top padding for the header,
  // bottom padding for the composer/sidebar). This handles both the status bar
  // and the gesture/3-button navigation bar reliably.
  @SuppressLint("AddJavascriptInterface")
  override fun onWebViewCreate(webView: WebView) {
    super.onWebViewCreate(webView)
    appWebView = webView
    webView.addJavascriptInterface(InsetsBridge(), "PiInsets")
    webView.addJavascriptInterface(BackBridge(), "PiBack")
    // FIX for issue 7 (IME language switcher dismisses keyboard):
    // Previously we returned WindowInsets.CONSUMED on API 30+, which tells the
    // system/WebView that we have fully handled the insets and they should not
    // be propagated to children. Consuming insets is known to cause WebView
    // focus loss during IME transitions: when the language picker (or other
    // IME overlay) triggers a WindowInsets dispatch with isVisible(IME)==false,
    // the consumed insets path causes the WebView to think the IME is gone and
    // it drops focus from the input, dismissing the keyboard. By returning the
    // original un-consumed insets, we let the platform/WebView retain its
    // default focus-preservation logic while still updating our own bottomInset
    // bookkeeping for the web layout.
    //
    // We also only push the pi-insets event to JS when values actually changed,
    // to avoid relayout churn from redundant dispatches that could also nudge
    // focus away. applyInsets now returns a Boolean (changed) instead of the
    // WindowInsets to consume.
    //
    // NOTE: This is a well-reasoned best guess without a device in this
    // environment. IME reporting varies by keyboard app (Gboard, Samsung, etc.)
    // and Android version. Device verification is required; if consuming is
    // still needed for status-bar drawing on some OEMs, we may need a more
    // targeted fix (e.g. only avoid consuming the IME type, or use
    // WindowInsetsController behavior).
    webView.setOnApplyWindowInsetsListener { _, insets ->
      val changed = applyInsets(insets)
      if (changed) {
        pushInsets(webView)
      }
      // Do NOT return CONSUMED; return the original insets so the WebView and
      // system can keep their default handling (especially focus preservation).
      // This is the key change for the language-switcher bug.
      insets
    }
    // The app opens its chat-list drawer with a left-edge swipe, but Android's
    // gesture-navigation back gesture also starts at the left edge and would
    // steal that swipe before the WebView ever sees a touch. Exclude a band
    // along the left edge from the system gesture so horizontal drags there get
    // delivered to the page. This is a best-effort hint (the OS caps how much
    // it honors), so the on-screen hamburger remains the guaranteed fallback.
    excludeLeftEdgeGesture()
  }

  private fun excludeLeftEdgeGesture() {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) return
    val density = resources.displayMetrics.density
    val band = Math.round(100f * density).toInt() // ~100dp band on the left edge
    val height = resources.displayMetrics.heightPixels
    val decor = window?.decorView ?: return
    decor.setSystemGestureExclusionRects(
      listOf(android.graphics.Rect(0, 0, band, height))
    )
  }

  /**
   * Update cached insets from the current WindowInsets and return whether
   * anything actually changed. Caller decides whether to push to JS and what
   * WindowInsets to propagate (we now propagate original, not CONSUMED).
   *
   * Reasoning for bottomInset (issue 7):
   * On API 30+ we read both systemBars+cutout (for top/nav) and IME (keyboard).
   * When isVisible(IME) is true, ime.bottom is the keyboard height and already
   * includes the nav bar beneath it. When false, we normally use bars.bottom
   * (nav bar only) — this is the collapsed state. However, showing the IME's
   * language/long-press picker causes a transient dispatch where isVisible goes
   * false for a single frame even though the keyboard is still logically up.
   * If we immediately collapse bottomInset, the web root's bottom padding
   * shrinks, the composer jumps, and the input loses focus -> keyboard
   * dismisses. We guard against this by requiring 2 consecutive hidden
   * dispatches before collapsing; a real dismiss (back press, tap outside)
   * quickly produces a second hidden, while the transient picker flicker is
   * a single-frame false between visible frames.
   *
   * We also only return "changed" when values differ, so pushInsets (which
   * triggers a JS relayout) does not run on redundant dispatches.
   */
  private fun applyInsets(insets: WindowInsets): Boolean {
    @Suppress("DEPRECATION")
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
      val bars = insets.getInsets(
        WindowInsets.Type.systemBars() or WindowInsets.Type.displayCutout()
      )
      val newTop = dp(bars.top)
      val ime = insets.getInsets(WindowInsets.Type.ime())
      val imeVisible = insets.isVisible(WindowInsets.Type.ime())
      val newBottom: Int
      if (imeVisible) {
        consecutiveImeHidden = 0
        // Keyboard is showing — reserve its full height (which includes the nav
        // bar beneath it, so there's no need to add bars.bottom as well).
        newBottom = dp(ime.bottom)
      } else {
        consecutiveImeHidden += 1
        // Heuristic for transient IME hide (language switcher):
        // If we were visible just before and this is the first hidden frame,
        // and the previous bottomInset looks keyboard-sized (significantly
        // larger than the nav bar), keep the old height for this frame.
        // A genuine dismiss will keep imeVisible==false and we collapse on
        // the second consecutive hidden.
        val prevWasKeyboard = bottomInset > dp(bars.bottom) + 40
        if (consecutiveImeHidden == 1 && wasImeVisible && prevWasKeyboard) {
          Log.d("MainActivity", "applyInsets: ignoring transient IME hide (bottom $bottomInset -> ${dp(bars.bottom)}, keeping)")
          newBottom = bottomInset
        } else {
          newBottom = dp(bars.bottom)
        }
      }
      wasImeVisible = imeVisible
      val newLeft = dp(bars.left)
      val newRight = dp(bars.right)
      val changed = newTop != topInset || newBottom != bottomInset || newLeft != leftInset || newRight != rightInset
      topInset = newTop
      bottomInset = newBottom
      leftInset = newLeft
      rightInset = newRight
      // Previously this returned WindowInsets.CONSUMED on this branch, which
      // is API 30+ only and signals that no child should see the insets.
      // That consumption is suspected to break WebView focus retention during
      // transient IME visibility toggles (language switcher). We now propagate
      // the original insets and let the system/WebView handle them; our own
      // bookkeeping above is sufficient for the web layout.
      return changed
    }
    // Legacy branch (< API 30). With `android:windowSoftInputMode="adjustResize"`
    // (set in the manifest) the activity is resized to sit above the IME, so the
    // IME height is already folded into `systemWindowInsetBottom` here — no
    // separate IME query is needed on these old versions.
    val newTop = dp(insets.systemWindowInsetTop)
    val newBottom = dp(insets.systemWindowInsetBottom)
    val newLeft = dp(insets.systemWindowInsetLeft)
    val newRight = dp(insets.systemWindowInsetRight)
    val changed = newTop != topInset || newBottom != bottomInset || newLeft != leftInset || newRight != rightInset
    topInset = newTop
    bottomInset = newBottom
    leftInset = newLeft
    rightInset = newRight
    return changed
  }

  private fun pushInsets(webView: WebView) {
    // Best-effort: dispatch to the page if it is loaded; if not, the app reads
    // the synchronous PiInsets values on mount anyway.
    webView.post {
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.KITKAT) {
        webView.evaluateJavascript(
          "window.dispatchEvent(new CustomEvent('pi-insets',{detail:{top:$topInset,bottom:$bottomInset,left:$leftInset,right:$rightInset}}))",
          null
        )
      }
    }
  }

  // Backed by `Java_com_rp_chat_MainActivity_initRustls` in src-tauri/src/android.rs;
  // the `this` receiver (an Activity, i.e. a Context) is passed as the JNI context.
  private external fun initRustls()

  companion object {
    // Dispatch a cancelable `pi-back` event and report whether a listener
    // consumed it (`defaultPrevented`). Wrapped in try/catch so a page that
    // isn't ready yet yields `false` (→ finish) instead of throwing.
    private const val BACK_JS =
      "(function(){try{var e=new CustomEvent('pi-back',{cancelable:true});" +
        "window.dispatchEvent(e);return e.defaultPrevented===true;}" +
        "catch(err){return false;}})()"

    init {
      // Ensure the native lib (which holds the initRustls export) is loaded
      // before the first call; idempotent alongside the `Rust` object's load.
      System.loadLibrary("pi_chat_lib")
    }
  }
}
