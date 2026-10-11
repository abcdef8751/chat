package com.rp.chat

import android.annotation.SuppressLint
import android.os.Build
import android.os.Bundle
import android.view.WindowInsets
import android.webkit.JavascriptInterface
import android.webkit.WebView
import androidx.activity.enableEdgeToEdge

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

  override fun onCreate(savedInstanceState: Bundle?) {
    // Edge-to-edge is enforced on Android 15+ (targetSdk 35+); the app draws
    // behind the status/nav bars. We translate the bars into insets exposed to
    // the web layer rather than relying on WebView padding (see onWebViewCreate).
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    // Register Android's system trust store with rustls-platform-verifier
    // (called on the main thread, single time). Without this, reqwest 0.13's
    // first HTTPS request SIGABRTs the process:
    // "Expect rustls-platform-verifier to be initialized".
    initRustls()
  }

  // WryActivity calls this once the WebView is created. We expose the system-bar
  // insets to the web content via `window.PiInsets` + a `pi-insets` event, and let
  // the app's own flex layout reserve the space (top padding for the header,
  // bottom padding for the composer/sidebar). This handles both the status bar
  // and the gesture/3-button navigation bar reliably.
  @SuppressLint("AddJavascriptInterface")
  override fun onWebViewCreate(webView: WebView) {
    super.onWebViewCreate(webView)
    webView.addJavascriptInterface(InsetsBridge(), "PiInsets")
    webView.setOnApplyWindowInsetsListener { _, insets ->
      val result = applyInsets(insets)
      pushInsets(webView)
      result
    }
  }

  private fun applyInsets(insets: WindowInsets): WindowInsets {
    @Suppress("DEPRECATION")
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
      val bars = insets.getInsets(
        WindowInsets.Type.systemBars() or WindowInsets.Type.displayCutout()
      )
      topInset = dp(bars.top)
      bottomInset = dp(bars.bottom)
      leftInset = dp(bars.left)
      rightInset = dp(bars.right)
      // `WindowInsets.CONSUMED` is API 30+ — only return it on this branch.
      return WindowInsets.CONSUMED
    }
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

  // Backed by `Java_com_rp_chat_MainActivity_initRustls` in src-tauri/src/android.rs;
  // the `this` receiver (an Activity, i.e. a Context) is passed as the JNI context.
  private external fun initRustls()

  companion object {
    init {
      // Ensure the native lib (which holds the initRustls export) is loaded
      // before the first call; idempotent alongside the `Rust` object's load.
      System.loadLibrary("pi_chat_lib")
    }
  }
}
