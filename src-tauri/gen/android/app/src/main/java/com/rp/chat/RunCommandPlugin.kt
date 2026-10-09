package com.rp.chat

import android.app.Activity
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.Build
import android.os.Bundle
import android.util.Log
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin

/**
 * Executes host shell tools (`bash`, `read_file`, `write_file`) **inside the
 * user's installed Termux** through Termux's public `RUN_COMMAND` intent, then
 * returns stdout/stderr/exit code to Rust (`crate::android`). See
 * `ANDROID_SHELL.md` for the architecture and rationale.
 *
 * Design notes:
 * - We cannot open Termux's data dir (per-app UID + SELinux), so every command
 *   runs on Termux's side via the intent.
 * - The result comes back through an `executionId`-scoped result `PendingIntent`
 *   (action `com.rp.chat.RUN_COMMAND_RESULT.<id>`) that Termux sends when the
 *   command finishes; a per-call `BroadcastReceiver` reads the Bundle and
 *   resolves the Rust `Invoke` (which awaits it on a oneshot channel).
 * - Requires Termux >= 0.112 for `RETURN_STDOUT`/`RETURN_STDERR` result extras.
 */
@TauriPlugin
class RunCommandPlugin(private val activity: Activity) : Plugin(activity) {

  companion object {
    private const val TAG = "RunCommandPlugin"
    private const val TERMUX = "com.termux"

    // Termux RunCommandService intent extras (public API).
    private const val EXTRA_PATH = "com.termux.RUN_COMMAND_PATH"
    private const val EXTRA_ARGUMENTS = "com.termux.RUN_COMMAND_ARGUMENTS"
    private const val EXTRA_WORKDIR = "com.termux.RUN_COMMAND_WORKDIR"
    private const val EXTRA_PENDING_INTENT = "com.termux.RUN_COMMAND_PENDING_INTENT"

    // Result extras Termux delivers to the result pending intent.
    private const val RESULT_STDOUT = "com.termux.RUN_COMMAND_STDOUT"
    private const val RESULT_STDERR = "com.termux.RUN_COMMAND_STDERR"
    private const val RESULT_EXIT_CODE = "com.termux.RUN_COMMAND_EXIT_CODE"

    // A result bundle is bounded; cap what we forward so a runaway command
    // cannot blow past the pending-intent result size (see ANDROID_SHELL.md).
    private const val MAX_OUTPUT_CHARS = 20_000
  }

  /** Termux's bash lives under its private `$PREFIX/bin/bash`. */
  private val termuxBash = "/data/data/com.termux/files/usr/bin/bash"

  @Command
  fun run(invoke: Invoke) {
    val args = invoke.parseArgs(RunArgs::class.java)
    val command = args.command
    val workdir = args.workdir

    if (!isTermuxInstalled()) {
      val err = JSObject()
      err.put("stdout", "")
      err.put("stderr", "bash: Termux is not installed — install Termux from F-Droid and grant it storage access.")
      err.put("exit_code", 1)
      invoke.resolve(err)
      return
    }

    val id = System.nanoTime()
    val callback = registerResultReceiver(id, workdir, invoke)
    val pending = buildResultPendingIntent(id)

    val intent = Intent(TERMUX + ".RUN_COMMAND").apply {
      setPackage(TERMUX)
      putExtra(EXTRA_PATH, termuxBash)
      putExtra(EXTRA_ARGUMENTS, arrayOf("-c", command))
      if (!workdir.isNullOrBlank()) putExtra(EXTRA_WORKDIR, workdir)
      putExtra(EXTRA_PENDING_INTENT, pending)
    }

    try {
      activity.startService(intent)
    } catch (e: Exception) {
      runCatching { activity.unregisterReceiver(callback) }
      invoke.reject("termux: failed to start RUN_COMMAND: ${e.message}")
    }
  }

  private fun isTermuxInstalled(): Boolean = try {
    activity.packageManager.getPackageInfo(TERMUX, 0) != null
  } catch (_: Exception) {
    false
  }

  private fun buildResultPendingIntent(id: Long): PendingIntent {
    val flags = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
      PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_MUTABLE
    } else {
      PendingIntent.FLAG_UPDATE_CURRENT
    }
    val resultIntent = Intent("com.rp.chat.RUN_COMMAND_RESULT.$id").apply {
      setPackage(activity.packageName)
    }
    return PendingIntent.getBroadcast(activity, id.toInt(), resultIntent, flags)
  }

  private fun registerResultReceiver(
    id: Long,
    workdir: String?,
    invoke: Invoke,
  ): BroadcastReceiver {
    val receiver = object : BroadcastReceiver() {
      override fun onReceive(context: Context, intent: Intent) {
        val stdout = intent.getStringExtra(RESULT_STDOUT).orEmpty()
        val stderr = intent.getStringExtra(RESULT_STDERR).orEmpty()
        val code = resultCode
        Log.d(TAG, "run($workdir) finished exit=$code")
        val ret = JSObject()
        ret.put("stdout", stdout.take(MAX_OUTPUT_CHARS))
        ret.put("stderr", stderr.take(MAX_OUTPUT_CHARS))
        ret.put("exit_code", code)
        invoke.resolve(ret)
      }
    }
    // Sending a broadcast PendingIntent delivers onReceive on the main thread.
    activity.registerReceiver(receiver, IntentFilter("com.rp.chat.RUN_COMMAND_RESULT.$id"))
    return receiver
  }
}

/** Args for `RunCommandPlugin.run`. */
data class RunArgs(val command: String?, val workdir: String?)
