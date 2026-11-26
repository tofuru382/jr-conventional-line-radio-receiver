package io.jrradio.receiver

import android.Manifest
import android.annotation.SuppressLint
import android.content.Intent
import android.content.pm.PackageManager
import android.hardware.usb.UsbManager
import android.graphics.Color
import android.os.Build
import android.os.Bundle
import android.view.View
import android.webkit.JavascriptInterface
import android.webkit.WebView
import androidx.activity.OnBackPressedCallback
import androidx.activity.SystemBarStyle
import androidx.activity.enableEdgeToEdge
import androidx.core.content.ContextCompat
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat

class MainActivity : TauriActivity() {
  // Black like the TIMS header and status bar in ui/style.css, so the system
  // bar insets blend with them.
  private val background = Color.parseColor("#000000")

  // While receiving, Back sends the app to the background instead of closing
  // the window (which would stop reception).
  private val keepReceivingOnBack =
    object : OnBackPressedCallback(false) {
      override fun handleOnBackPressed() {
        moveTaskToBack(true)
      }
    }

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge(
      statusBarStyle = SystemBarStyle.dark(background),
      navigationBarStyle = SystemBarStyle.dark(background),
    )
    super.onCreate(savedInstanceState)
    UsbDongle.persist(this)
    // Read before reception can start again in this process.
    lastKill = ReceiverService.lastKill(this)
    if (lastKill.isNotEmpty()) UsbDongle.note("Android: 前回の受信は途中でアプリごと止められていました")
    UsbDongle.watch(this)
    window.decorView.setBackgroundColor(background)
    // Android 15+ always draws apps edge to edge. Keep the WebView clear of the
    // status bar, navigation bar, display cutout and on-screen keyboard.
    val content = findViewById<View>(android.R.id.content)
    ViewCompat.setOnApplyWindowInsetsListener(content) { view, insets ->
      val bars =
        insets.getInsets(
          WindowInsetsCompat.Type.systemBars() or
            WindowInsetsCompat.Type.displayCutout() or
            WindowInsetsCompat.Type.ime(),
        )
      view.setPadding(bars.left, bars.top, bars.right, bars.bottom)
      WindowInsetsCompat.CONSUMED
    }
    onBackPressedDispatcher.addCallback(this, keepReceivingOnBack)
    handleUsbAttach(intent)
  }

  // A dongle plugged in (again). With "use by default" chosen, Android grants
  // USB access here, which lets the built-in driver recover from a USB drop.
  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    handleUsbAttach(intent)
  }

  private fun handleUsbAttach(intent: Intent?) {
    if (intent?.action == UsbManager.ACTION_USB_DEVICE_ATTACHED && UsbDongle.sessionWantsDevice()) {
      UsbDongle.request(this)
    }
  }

  private var lastKill = ""

  override fun onStart() {
    super.onStart()
    UsbDongle.note("Android: アプリを表示しました")
  }

  override fun onStop() {
    UsbDongle.note("Android: アプリがバックグラウンドになりました")
    super.onStop()
  }

  @SuppressLint("JavascriptInterface")
  override fun onWebViewCreate(webView: WebView) {
    // Only the app's own bundled page is ever loaded in this WebView.
    webView.addJavascriptInterface(Bridge(), "JrAndroid")
  }

  inner class Bridge {
    /** Called by ui/app.js whenever reception starts or ends. */
    @JavascriptInterface
    fun setReceiving(receiving: Boolean) {
      runOnUiThread {
        keepReceivingOnBack.isEnabled = receiving
        if (receiving) {
          requestNotificationPermission()
          ReceiverService.start(this@MainActivity)
        } else {
          ReceiverService.stop(this@MainActivity)
          UsbDongle.close()
        }
      }
    }

    /**
     * Built-in driver: find the dongle, ask for USB access if needed and hand
     * it to the receiver. Returns "ok", "asking", "none", "other:N" or "busy"
     * (see [UsbDongle.request]).
     */
    @JavascriptInterface
    fun requestUsb(): String = UsbDongle.request(this@MainActivity, user = true)

    /** Reception did not start after [requestUsb]: let go of the dongle. */
    @JavascriptInterface
    fun releaseUsb() = UsbDongle.close()

    /** "Listen" turned on or off while receiving: audio focus, media session. */
    @JavascriptInterface
    fun setListening(on: Boolean) {
      runOnUiThread { AudioRoute.setListening(this@MainActivity, on) }
    }

    /** The page's recording player stopped: take the audio focus back (see [AudioRoute.reclaim]). */
    @JavascriptInterface
    fun reclaimAudioFocus() {
      runOnUiThread { AudioRoute.reclaim(this@MainActivity) }
    }

    /** Headphone, audio focus and media button counters (see [AudioRoute.state]). */
    @JavascriptInterface fun audioRoute(): String = AudioRoute.state()

    /**
     * A finished recording folder: index its files so music and file apps
     * show them at once (files written by path are not in MediaStore yet).
     */
    @JavascriptInterface
    fun scanRecordings(path: String) {
      val files =
        java.io.File(path).listFiles()?.filter { it.isFile }?.map { it.absolutePath }?.toTypedArray()
          ?: return
      if (files.isNotEmpty()) android.media.MediaScannerConnection.scanFile(this@MainActivity, files, null, null)
    }

    /** The notification's band map layout (see [NativeBand.layout]). */
    @JavascriptInterface
    fun setBandLayout(json: String) {
      NativeBand.layout = json
    }

    /** Whether the phone sees the dongle on USB right now. */
    @JavascriptInterface fun usbAttached(): Boolean = UsbDongle.find(this@MainActivity) != null

    /** What the phone saw of the dongle, for a lost-dongle error's diagnosis. */
    @JavascriptInterface fun usbDiagnosis(): String = UsbDongle.diagnosis(this@MainActivity)

    /** Android-side connection notes, one "<unix ms>\t<text>" per line. */
    @JavascriptInterface fun usbLog(): String = UsbDongle.notes()

    /** The previous run's Android notes (it may have been killed). */
    @JavascriptInterface fun usbLogPrevious(): String = UsbDongle.previousNotes(this@MainActivity)

    /** "<start ms>\t<last sign of life ms>" when the previous run was killed while receiving. */
    @JavascriptInterface fun lastKill(): String = this@MainActivity.lastKill
  }

  private fun requestNotificationPermission() {
    if (
      Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
        ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS) !=
          PackageManager.PERMISSION_GRANTED
    ) {
      // Without it the service still runs; only its notification is hidden.
      requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS)) {}
    }
  }
}
