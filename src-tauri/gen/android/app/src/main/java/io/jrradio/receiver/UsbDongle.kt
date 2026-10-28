package io.jrradio.receiver

import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbDeviceConnection
import android.hardware.usb.UsbManager
import android.os.Build
import android.util.Log
import androidx.core.content.ContextCompat

/**
 * Built-in RTL-SDR driver, Android side. Apps cannot open USB devices from
 * native code, so this finds the dongle, asks for permission, opens it and
 * hands the file descriptor to the Rust driver (receiver-core `usb`), which
 * duplicates it. The connection is kept open until the next one replaces it.
 */
object UsbDongle {
  private const val TAG = "UsbDongle"
  private const val ACTION_PERMISSION = "io.jrradio.receiver.USB_PERMISSION"
  // RTL2832U dongles (RTL-SDR Blog V3/V4 and most others).
  private const val VENDOR = 0x0bda
  private val PRODUCTS = setOf(0x2832, 0x2838)

  private var connection: UsbDeviceConnection? = null
  private var asking = false
  private var receiverRegistered = false

  /**
   * Implemented in Rust (src-tauri/src/lib.rs, `usb_jni`). [identity]: 1 =
   * RTL-SDR Blog V4, 2 = another dongle, 0 = unknown. The V4 needs its own
   * input switching at 350 MHz, and Android already read the USB strings.
   */
  @JvmStatic external fun offerFd(fd: Int, identity: Int)

  @JvmStatic external fun wantsDevice(): Boolean

  /** False until the Rust library is loaded (it is, once the app has started). */
  fun sessionWantsDevice(): Boolean =
    try {
      wantsDevice()
    } catch (e: UnsatisfiedLinkError) {
      false
    }

  private fun manager(context: Context) = context.getSystemService(Context.USB_SERVICE) as UsbManager

  fun find(context: Context): UsbDevice? =
    manager(context).deviceList.values.firstOrNull {
      it.vendorId == VENDOR && it.productId in PRODUCTS
    }

  /**
   * Makes the dongle available to the driver: "ok" (handed over), "asking"
   * (permission dialog shown; handed over once granted) or "none" (no dongle).
   */
  @Synchronized
  fun request(context: Context): String {
    val device = find(context) ?: return "none"
    val usb = manager(context)
    if (usb.hasPermission(device)) {
      return if (open(context, device)) "ok" else "none"
    }
    if (!asking) {
      asking = true
      registerPermissionReceiver(context.applicationContext)
      val flags =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) PendingIntent.FLAG_MUTABLE else 0
      val intent = Intent(ACTION_PERMISSION).setPackage(context.packageName)
      usb.requestPermission(device, PendingIntent.getBroadcast(context, 0, intent, flags))
    }
    return "asking"
  }

  @Synchronized
  private fun open(context: Context, device: UsbDevice): Boolean {
    val opened = manager(context).openDevice(device) ?: return false
    connection?.close()
    connection = opened
    val identity =
      try {
        val manufacturer = device.manufacturerName
        val product = device.productName
        Log.i(TAG, "dongle: $manufacturer / $product")
        when {
          manufacturer == null && product == null -> 0
          manufacturer == "RTLSDRBlog" && product == "Blog V4" -> 1
          else -> 2
        }
      } catch (e: SecurityException) {
        0
      }
    return try {
      offerFd(opened.fileDescriptor, identity)
      true
    } catch (e: UnsatisfiedLinkError) {
      Log.w(TAG, "driver library not loaded", e)
      false
    }
  }

  /** Reception stopped: release the device for other apps. */
  @Synchronized
  fun close() {
    connection?.close()
    connection = null
  }

  private fun registerPermissionReceiver(context: Context) {
    if (receiverRegistered) return
    receiverRegistered = true
    ContextCompat.registerReceiver(
      context,
      object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
          synchronized(this@UsbDongle) { asking = false }
          val granted = intent.getBooleanExtra(UsbManager.EXTRA_PERMISSION_GRANTED, false)
          val device = find(context)
          if (granted && device != null) open(context, device)
        }
      },
      IntentFilter(ACTION_PERMISSION),
      ContextCompat.RECEIVER_NOT_EXPORTED,
    )
  }
}
