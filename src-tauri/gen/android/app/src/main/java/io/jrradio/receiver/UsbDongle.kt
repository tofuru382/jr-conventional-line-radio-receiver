package io.jrradio.receiver

import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbDeviceConnection
import android.hardware.usb.UsbManager
import android.os.BatteryManager
import android.os.Build
import android.os.SystemClock
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
  // RTL2832U dongles by USB ID: librtlsdr's list of known devices (RTL-SDR
  // Blog V3/V4 and most others use 0x0bda). The built-in driver needs an
  // R820T/R828D tuner and says so when a dongle has another one. Keep in step
  // with rtlsdr-nusb's KNOWN_DEVICES and res/xml/rtl_sdr_devices.xml.
  private val KNOWN =
    setOf(
      0x0bda to 0x2832,
      0x0bda to 0x2838,
      0x0413 to 0x6680,
      0x0413 to 0x6f0f,
      0x0458 to 0x707f,
      0x0ccd to 0x00a9,
      0x0ccd to 0x00b3,
      0x0ccd to 0x00b4,
      0x0ccd to 0x00b5,
      0x0ccd to 0x00b7,
      0x0ccd to 0x00b8,
      0x0ccd to 0x00b9,
      0x0ccd to 0x00c0,
      0x0ccd to 0x00c6,
      0x0ccd to 0x00d3,
      0x0ccd to 0x00d7,
      0x0ccd to 0x00e0,
      0x1554 to 0x5020,
      0x15f4 to 0x0131,
      0x15f4 to 0x0133,
      0x185b to 0x0620,
      0x185b to 0x0650,
      0x185b to 0x0680,
      0x1b80 to 0xd393,
      0x1b80 to 0xd394,
      0x1b80 to 0xd395,
      0x1b80 to 0xd397,
      0x1b80 to 0xd398,
      0x1b80 to 0xd39d,
      0x1b80 to 0xd3a4,
      0x1b80 to 0xd3a8,
      0x1b80 to 0xd3af,
      0x1b80 to 0xd3b0,
      0x1d19 to 0x1101,
      0x1d19 to 0x1102,
      0x1d19 to 0x1103,
      0x1d19 to 0x1104,
      0x1f4d to 0xa803,
      0x1f4d to 0xb803,
      0x1f4d to 0xc803,
      0x1f4d to 0xd286,
      0x1f4d to 0xd803,
    )

  private fun isDongle(device: UsbDevice) = (device.vendorId to device.productId) in KNOWN

  private var connection: UsbDeviceConnection? = null
  private var receiverRegistered = false
  private var watching = false

  // The USB permission dialog. Android answers through [ACTION_PERMISSION],
  // but not always: a dialog that disappears without an answer (the dongle
  // dropped off USB while it was shown, or the phone never showed it) left
  // the app waiting for good, asking nothing until it was restarted. So an
  // unanswered request is repeated after ASK_AGAIN_MS, at once for another
  // device instance, and after 3 s when the person presses start again.
  private const val ASK_AGAIN_MS = 15000L
  private const val ASK_AGAIN_USER_MS = 3000L
  // After "deny", the waiting session does not pop the dialog up again at
  // once (it would every second); pressing start asks again.
  private const val DENIED_PAUSE_MS = 10000L
  private var asking = false
  private var askedAt = 0L
  private var askedFor = -1
  private var deniedAt = 0L
  private var waitNoted = 0L

  /** Android-side notes for the app's connection log (ui/app.js merges them). */
  private val notes = ArrayDeque<String>()

  // The notes also go to a file, so those of a run the phone killed in the
  // background can be read after the next start (previous run: .prev.log).
  @Volatile private var logFile: java.io.File? = null

  /** Starts this run's log file; called once per process. */
  fun persist(context: Context) {
    if (logFile != null) return
    val file = java.io.File(context.filesDir, "android-connection.log")
    if (file.exists()) file.renameTo(java.io.File(context.filesDir, "android-connection.prev.log"))
    logFile = file
  }

  /** The previous run's notes, one "<unix ms>\t<text>" per line. */
  fun previousNotes(context: Context): String =
    try {
      java.io.File(context.filesDir, "android-connection.prev.log").readText()
    } catch (e: Exception) {
      ""
    }

  fun note(text: String) {
    Log.i(TAG, text)
    val line = "${System.currentTimeMillis()}\t${text.replace('\n', ' ')}"
    synchronized(notes) {
      if (notes.size == 200) notes.removeFirst()
      notes.addLast(line)
      logFile?.let { file ->
        try {
          if (file.length() > 256 * 1024) file.writeText(file.readLines().takeLast(500).joinToString("\n", postfix = "\n"))
          file.appendText(line + "\n")
        } catch (e: Exception) {
          Log.w(TAG, "connection log not written", e)
        }
      }
    }
  }

  /** When Android last reported a USB device detached (elapsedRealtime), or 0. */
  @Volatile private var detachedAt = 0L

  // A dongle whose plug is losing contact comes and goes within a second.
  // Opening it at once makes it draw its full current through that contact,
  // and after a few such bounces some phones turn the USB port off until the
  // dongle is replugged. So a dongle that just appeared is opened only once
  // it has stayed attached for a while: 2.5 s, doubled for every further
  // detach in the last minute (at most 10 s; 20 s kept it out of use too
  // long on a plug that bounces often).
  private const val SETTLE_MS = 2500L
  private const val SETTLE_MAX_MS = 10000L
  private const val BOUNCE_WINDOW_MS = 60000L
  @Volatile private var attachedAt = 0L
  private val detaches = ArrayDeque<Long>()
  private var settlingNoted = 0L
  private var lastMissing: String? = null
  private var lastMissingNoted = 0L

  /** How long a dongle must stay attached before it is opened (ms). */
  internal fun settleTime(now: Long, detachTimes: List<Long>): Long {
    val recent = detachTimes.count { now - it <= BOUNCE_WINDOW_MS }
    if (recent <= 1) return SETTLE_MS
    return (SETTLE_MS shl minOf(recent - 1, 3)).coerceAtMost(SETTLE_MAX_MS)
  }

  /** Milliseconds left before a dongle that just appeared may be opened. */
  private fun settling(): Long {
    if (attachedAt == 0L) return 0
    val now = SystemClock.elapsedRealtime()
    val wait = synchronized(detaches) { settleTime(now, detaches.toList()) }
    return (attachedAt + wait - now).coerceAtLeast(0)
  }

  /** "電池 38.5℃ 62%": heat or a low battery can make a phone cut USB power. */
  private fun battery(context: Context): String {
    val status =
      context.applicationContext.registerReceiver(null, IntentFilter(Intent.ACTION_BATTERY_CHANGED))
        ?: return "電池 不明"
    val temperature = status.getIntExtra(BatteryManager.EXTRA_TEMPERATURE, Int.MIN_VALUE)
    val level = status.getIntExtra(BatteryManager.EXTRA_LEVEL, -1)
    val scale = status.getIntExtra(BatteryManager.EXTRA_SCALE, -1)
    val degrees = if (temperature == Int.MIN_VALUE) "?" else "%.1f".format(java.util.Locale.ROOT, temperature / 10.0)
    val percent = if (level < 0 || scale <= 0) "?" else (level * 100 / scale).toString()
    return "電池 ${degrees}℃ ${percent}%"
  }

  /**
   * What the phone saw, for the diagnosis shown with a lost-dongle error:
   * whether Android itself reported the USB device detached (power or
   * contact; nothing an app can undo) or the dongle is still attached (it
   * stopped answering), and the battery state.
   */
  fun diagnosis(context: Context): String {
    val attached = manager(context).deviceList.values
    val now =
      when {
        attached.any { isDongle(it) } -> "今はドングルあり"
        attached.isEmpty() -> "今は USB 機器なし"
        else -> "今はドングルなし（ほかの USB 機器 ${attached.size} 台）"
      }
    val detached =
      if (detachedAt == 0L) "スマホは USB の切断を検出していない"
      else "スマホが USB の切断を検出（${(SystemClock.elapsedRealtime() - detachedAt) / 1000} 秒前）"
    return "$detached · $now · ${battery(context)}"
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
          val detached = intent.action == UsbManager.ACTION_USB_DEVICE_DETACHED
          val now = SystemClock.elapsedRealtime()
          if (detached) {
            detachedAt = now
            synchronized(detaches) {
              detaches.addLast(now)
              while (detaches.isNotEmpty() && now - detaches.first() > BOUNCE_WINDOW_MS) detaches.removeFirst()
            }
          } else if (device == null || isDongle(device)) {
            attachedAt = now
          }
          val what = if (detached) "外れました" else "つながりました"
          note("Android: USB 機器が$what（${device?.let { describe(it) } ?: "不明"}）· ${battery(context)}")
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
    manager(context).deviceList.values.firstOrNull { isDongle(it) }

  /**
   * Makes the dongle available to the driver: "ok" (handed over), "asking"
   * (permission dialog shown; handed over once granted), "none" (no USB device
   * at all), "other:N" (N USB devices, none an RTL-SDR, e.g. only a hub),
   * "settling" (it just appeared; asked again until it stayed attached) or
   * "busy" (the dongle is there but could not be opened). [user]: the
   * person pressed start (the page asks), not the waiting session.
   */
  @Synchronized
  fun request(context: Context, user: Boolean = false): String {
    val usb = manager(context)
    val attached = usb.deviceList.values
    val device = attached.firstOrNull { isDongle(it) }
    if (device == null) {
      val result = if (attached.isEmpty()) "none" else "other:${attached.size}"
      // Asked every second while a session waits: note changes, and every 10 s.
      val now = SystemClock.elapsedRealtime()
      if (result != lastMissing || now - lastMissingNoted >= 10000) {
        lastMissing = result
        lastMissingNoted = now
        note("Android: ドングルを探しました: USB 機器 ${attached.size} 台 ${attached.joinToString { describe(it) }} → $result")
      }
      return result
    }
    lastMissing = null
    val wait = settling()
    if (wait > 0) {
      val now = SystemClock.elapsedRealtime()
      if (now - settlingNoted > 1500) {
        settlingNoted = now
        note("Android: ドングルがつながったばかりなので、安定するまで待ちます（あと ${(wait + 999) / 1000} 秒）")
      }
      return "settling"
    }
    if (usb.hasPermission(device)) {
      val result = if (open(context, device)) "ok" else "busy"
      // An earlier detach is history once the dongle is open again.
      if (result == "ok") detachedAt = 0L
      note("Android: ドングルを開きました（${describe(device)}）→ $result")
      return result
    }
    val now = SystemClock.elapsedRealtime()
    if (user) deniedAt = 0L
    val unanswered =
      asking &&
        (askedFor != device.deviceId || now - askedAt >= (if (user) ASK_AGAIN_USER_MS else ASK_AGAIN_MS))
    if (!asking && deniedAt != 0L && now - deniedAt < DENIED_PAUSE_MS) return "asking"
    if (!asking || unanswered) {
      note(
        if (unanswered) "Android: USB の使用許可の確認に答えがないので、もう一度確認します（${describe(device)}）"
        else "Android: USB の使用許可を求めます（${describe(device)}）"
      )
      asking = true
      askedAt = now
      askedFor = device.deviceId
      registerPermissionReceiver(context.applicationContext)
      val flags =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) PendingIntent.FLAG_MUTABLE else 0
      val intent = Intent(ACTION_PERMISSION).setPackage(context.packageName)
      usb.requestPermission(device, PendingIntent.getBroadcast(context, 0, intent, flags))
    } else if (now - waitNoted >= 10000) {
      // Asked every second while a session waits: a note every 10 s.
      waitNoted = now
      note("Android: USB の使用許可の答えを待っています（${(now - askedAt) / 1000} 秒）")
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
          val granted = intent.getBooleanExtra(UsbManager.EXTRA_PERMISSION_GRANTED, false)
          synchronized(this@UsbDongle) {
            asking = false
            deniedAt = if (granted) 0L else SystemClock.elapsedRealtime()
          }
          note("Android: USB の使用許可: ${if (granted) "許可" else "拒否"}")
          val device = find(context)
          // Only for a session that waits: a descriptor handed over after
          // reception stopped would be stale by the next start.
          if (granted && device != null && sessionWantsDevice()) open(context, device)
        }
      },
      IntentFilter(ACTION_PERMISSION),
      ContextCompat.RECEIVER_NOT_EXPORTED,
    )
  }
}
