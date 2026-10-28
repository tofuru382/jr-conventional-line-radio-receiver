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
  private var watching = false

  /** Android-side notes for the app's connection log (ui/app.js merges them). */
  private val notes = ArrayDeque<String>()

  fun note(text: String) {
    Log.i(TAG, text)
    synchronized(notes) {
      if (notes.size == 200) notes.removeFirst()
      notes.addLast("${System.currentTimeMillis()}\t$text")
    }
  }

  /** One note per line: "<unix ms>\t<text>". */
  fun notes(): String = synchronized(notes) { notes.joinToString("\n") }

  private fun describe(device: UsbDevice): String {
    val names =
      try {
        listOfNotNull(device.manufacturerName, device.productName).joinToString(" ")
      } catch (e: SecurityException) {
        ""
      }
    return "%04x:%04x%s".format(device.vendorId, device.productId, if (names.isEmpty()) "" else " $names")
  }

  /** Notes every dongle attach and detach, to tell a dropped USB link from a driver error. */
  fun watch(context: Context) {
    synchronized(this) {
      if (watching) return
      watching = true
    }
    val filter =
      IntentFilter().apply {
        addAction(UsbManager.ACTION_USB_DEVICE_ATTACHED)
        addAction(UsbManager.ACTION_USB_DEVICE_DETACHED)
      }
    ContextCompat.registerReceiver(
      context.applicationContext,
      object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
          @Suppress("DEPRECATION")
          val device = intent.getParcelableExtra<UsbDevice>(UsbManager.EXTRA_DEVICE)
          val what = if (intent.action == UsbManager.ACTION_USB_DEVICE_DETACHED) "外れました" else "つながりました"
          note("Android: USB 機器が$what（${device?.let { describe(it) } ?: "不明"}）")
        }
      },
      filter,
      // Both are protected system broadcasts.
      ContextCompat.RECEIVER_EXPORTED,
    )
    val attached = manager(context).deviceList.values
    note("Android: アプリ起動時の USB 機器 ${attached.size} 台 ${attached.joinToString { describe(it) }}")
  }

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
   * (permission dialog shown; handed over once granted), "none" (no USB device
   * at all), "other:N" (N USB devices, none an RTL-SDR, e.g. only a hub) or
   * "busy" (the dongle is there but could not be opened).
   */
  @Synchronized
  fun request(context: Context): String {
    val usb = manager(context)
    val attached = usb.deviceList.values
    val device = attached.firstOrNull { it.vendorId == VENDOR && it.productId in PRODUCTS }
    if (device == null) {
      val result = if (attached.isEmpty()) "none" else "other:${attached.size}"
      note("Android: ドングルを探しました: USB 機器 ${attached.size} 台 ${attached.joinToString { describe(it) }} → $result")
      return result
    }
    if (usb.hasPermission(device)) {
      val result = if (open(context, device)) "ok" else "busy"
      note("Android: ドングルを開きました（${describe(device)}）→ $result")
      return result
    }
    note("Android: USB の使用許可を求めます（${describe(device)}）")
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
    val opened =
      manager(context).openDevice(device)
        ?: run {
          note("Android: openDevice に失敗しました（${device.deviceName}）")
          return false
        }
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
    if (connection != null) note("Android: 受信を止めたのでドングルを放します")
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
          note("Android: USB の使用許可: ${if (granted) "許可" else "拒否"}")
          val device = find(context)
          if (granted && device != null) open(context, device)
        }
      },
      IntentFilter(ACTION_PERMISSION),
      ContextCompat.RECEIVER_NOT_EXPORTED,
    )
  }
}
