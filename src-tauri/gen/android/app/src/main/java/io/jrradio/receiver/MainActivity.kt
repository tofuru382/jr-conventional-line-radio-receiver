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
  // Matches --bg in ui/style.css so the inset areas blend with the page.
  private val background = Color.parseColor("#0d141b")

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
    fun requestUsb(): String = UsbDongle.request(this@MainActivity)
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
