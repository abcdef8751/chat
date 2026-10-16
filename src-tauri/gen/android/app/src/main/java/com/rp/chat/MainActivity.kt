package com.rp.chat

import android.annotation.SuppressLint
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.util.Log
import android.view.View
import android.view.ViewGroup
import android.view.WindowInsets
import android.webkit.JavascriptInterface
import android.webkit.WebView
import android.widget.FrameLayout
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
    webView.setOnApplyWindowInsetsListener { _, insets ->
      val result = applyInsets(insets)
      pushInsets(webView)
      result
    }
    // The app opens its chat-list drawer with a left-edge swipe, but Android's
    // gesture-navigation back gesture also starts at the left edge and would
    // steal that swipe before the WebView ever sees a touch. Exclude a band
    // along the left edge from the system gesture so horizontal drags there get
    // delivered to the page. This is a best-effort hint (the OS caps how much
    // it honors), so the on-screen hamburger remains the guaranteed fallback.
    excludeLeftEdgeGesture()
    // Shield the WebView from transient window-focus loss (issue 7). Installed
    // last so the WebView is already in the hierarchy.
    installFocusShield(webView)
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

  private fun applyInsets(insets: WindowInsets): WindowInsets {
    @Suppress("DEPRECATION")
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
      val bars = insets.getInsets(
        WindowInsets.Type.systemBars() or WindowInsets.Type.displayCutout()
      )
      topInset = dp(bars.top)
      // Bottom: use the IME (soft keyboard) inset when it is visible and taller
      // than the nav bar, so the whole app (messages + composer) is raised above
      // the keyboard instead of the keyboard drawing on top of it. The frontend
      // root already reserves `bottomInset` as CSS px, so growing this value is
      // all that is needed — the web layer re-lays out the flex column for us.
      val ime = insets.getInsets(WindowInsets.Type.ime())
      // `isVisible(t)` is a method on the root insets (API 30+); the WindowInsets
      // returned by getInsets has no no-arg `isVisible` property.
      if (insets.isVisible(WindowInsets.Type.ime())) {
        // Keyboard is showing — reserve its full height (which includes the nav
        // bar beneath it, so there's no need to add bars.bottom as well).
        bottomInset = dp(ime.bottom)
      } else {
        bottomInset = dp(bars.bottom)
      }
      leftInset = dp(bars.left)
      rightInset = dp(bars.right)
      // `WindowInsets.CONSUMED` is API 30+ — only return it on this branch.
      return WindowInsets.CONSUMED
    }
    // Legacy branch (< API 30). With `android:windowSoftInputMode="adjustResize"`
    // (set in the manifest) the activity is resized to sit above the IME, so the
    // IME height is already folded into `systemWindowInsetBottom` here — no
    // separate IME query is needed on these old versions.
    topInset = dp(insets.systemWindowInsetTop)
    bottomInset = dp(insets.systemWindowInsetBottom)
    leftInset = dp(insets.systemWindowInsetLeft)
    rightInset = dp(insets.systemWindowInsetRight)
    // On < API 30 there is no `CONSUMED` constant; returning the original (not
    // consumed) is correct since the WebView has no inset-aware children.
    return insets
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

  /**
   * Wrap the WebView in a [FocusShieldLayout].
   *
   * Issue 7. Chromium's WebView hides the soft keyboard the moment it observes
   * `onWindowFocusChanged(false)` — verified on-device: the hide lands ~6 ms
   * after `MainActivity handleWindowFocusChanged: 0`, before any app code runs.
   * An IME that presents its language picker as a *focus-stealing dialog*
   * (FUTO's is a TYPE_APPLICATION_ATTACHED_DIALOG) therefore gets that dialog
   * torn down the instant it opens, because the host window loses focus. Native
   * apps are unaffected — Samsung Settings keeps the keyboard up through the very
   * same dialog — so the fix is to stop the WebView from ever seeing the
   * transient focus loss.
   */
  private fun installFocusShield(webView: WebView, attempt: Int = 0) {
    val parent = webView.parent as? ViewGroup
    if (parent == null) {
      // Tauri may add the WebView to the content view just after this hook.
      if (attempt < 5) webView.post { installFocusShield(webView, attempt + 1) }
      return
    }
    if (parent is FocusShieldLayout) return // already installed
    val lp = webView.layoutParams
    val index = parent.indexOfChild(webView)
    parent.removeViewAt(index)
    val shield = FocusShieldLayout(this)
    shield.layoutParams = lp
    // The WebView keeps filling the shield, which in turn keeps the WebView's
    // original layout params, so the visual result is unchanged.
    shield.addView(
      webView,
      ViewGroup.LayoutParams.MATCH_PARENT,
      ViewGroup.LayoutParams.MATCH_PARENT,
    )
    parent.addView(shield, index)
  }

  /**
   * A FrameLayout that keeps a transient window-focus LOSS from reaching its
   * children.
   *
   * While the activity's window is still visible, focus moving to another window
   * means an IME's own dialog stole it (nothing else legitimately takes focus
   * from a foreground activity). Forwarding that loss makes Chromium hide the
   * keyboard, which in turn destroys the IME's dialog — the language list never
   * gets a chance to render. Once the window is no longer visible (home/recents)
   * the loss is reported as usual, so backgrounding behaviour is unchanged.
   */
  private class FocusShieldLayout(context: Context) : FrameLayout(context) {
    override fun dispatchWindowFocusChanged(hasFocus: Boolean) {
      // `windowVisibility` (public API) is the visibility of this view's window.
      // `View.getWindow()` is a hidden method and is unavailable to the Kotlin
      // compiler, so it cannot be used here.
      val windowVisible = windowVisibility == View.VISIBLE
      super.dispatchWindowFocusChanged(hasFocus || windowVisible)
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
