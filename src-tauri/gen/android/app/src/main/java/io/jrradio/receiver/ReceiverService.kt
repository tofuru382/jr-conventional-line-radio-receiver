package io.jrradio.receiver

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.BroadcastReceiver
import android.content.ComponentCallbacks2
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.PowerManager
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat

/**
 * Keeps the process and CPU running while receiving, so decoding, recording
 * and the native audio monitor continue with the screen off or the app in the
 * background. The receiver itself runs in Rust; this service only holds the
 * foreground state, a partial wake lock and the ongoing notification.
 */
class ReceiverService : Service() {
  private var wakeLock: PowerManager.WakeLock? = null
  private val handler = Handler(Looper.getMainLooper())

  // Built-in driver: while a session waits for the dongle (it dropped off USB
  // and came back), hand it over again, also with the app in the background.
  // Every second, and right when a settling dongle may be opened (the
  // second's wait came on top of it).
  private val usbWatch =
    object : Runnable {
      override fun run() {
        if (UsbDongle.sessionWantsDevice()) UsbDongle.request(this@ReceiverService)
        handler.postDelayed(this, UsbDongle.askAgainIn())
      }
    }

  // The band map in the notification, refreshed once a second (only when
  // something changed), with the app in the background too.
  private var shown = ""
  private val bandUpdate =
    object : Runnable {
      override fun run() {
        val content = NativeBand.content()
        val signature = content?.signature ?: ""
        if (signature != shown) {
          shown = signature
          (getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager)
            .notify(NOTIFICATION_ID, notification(content))
        }
        handler.postDelayed(this, 1000)
      }
    }

  // While receiving, a timestamp every 30 s: a run the phone killed leaves
  // the last one behind (see [lastKill]).
  private val heartbeat =
    object : Runnable {
      override fun run() {
        prefs(this@ReceiverService).edit().putLong(KEY_ALIVE, System.currentTimeMillis()).apply()
        handler.postDelayed(this, 30000)
      }
    }

  // Screen on/off in the connection log: whether reception stops with the
  // screen, not with the app leaving the screen.
  private val screen =
    object : BroadcastReceiver() {
      override fun onReceive(context: Context, intent: Intent) {
        val on = intent.action == Intent.ACTION_SCREEN_ON
        UsbDongle.note("Android: 画面が${if (on) "つきました" else "消えました"}")
      }
    }
  private var screenWatched = false

  override fun onBind(intent: Intent?): IBinder? = null

  override fun onTaskRemoved(rootIntent: Intent?) {
    UsbDongle.note("Android: アプリが最近のアプリ一覧から消されました（受信は続けます）")
    super.onTaskRemoved(rootIntent)
  }

  override fun onTrimMemory(level: Int) {
    super.onTrimMemory(level)
    if (level >= ComponentCallbacks2.TRIM_MEMORY_RUNNING_LOW) {
      UsbDongle.note("Android: メモリが足りないと通知されました（レベル $level）")
    }
  }

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    if (intent?.action == ACTION_STOP) {
      // The notification's stop button; the page may be gone.
      try {
        NativeBand.stop()
      } catch (e: UnsatisfiedLinkError) {
        // The library is always loaded while the service runs.
      }
      UsbDongle.close()
      stopForeground(STOP_FOREGROUND_REMOVE)
      stopSelf()
      return START_NOT_STICKY
    }
    // Playing the monitor and driving a USB dongle (or a network tuner):
    // both describe this service; the second also covers receiving with the
    // monitor muted. Its prerequisite is met by CHANGE_NETWORK_STATE (and,
    // for the built-in driver, the USB permission). Should a phone refuse
    // the combination, media playback alone is used as before.
    shown = ""
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
      try {
        ServiceCompat.startForeground(
          this,
          NOTIFICATION_ID,
          notification(null),
          ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK or
            ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE,
        )
      } catch (e: RuntimeException) {
        UsbDongle.note("Android: サービス種別の追加を拒否されました（${e.javaClass.simpleName}）")
        ServiceCompat.startForeground(
          this,
          NOTIFICATION_ID,
          notification(null),
          ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK,
        )
      }
    } else {
      ServiceCompat.startForeground(this, NOTIFICATION_ID, notification(null), 0)
    }
    AudioRoute.watch(this)
    watchThermal()
    if (wakeLock == null) {
      val power = getSystemService(Context.POWER_SERVICE) as PowerManager
      wakeLock =
        power.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "jrradio:receiver").apply {
          setReferenceCounted(false)
          acquire()
        }
    }
    handler.removeCallbacks(usbWatch)
    // A start whose dongle was settling waited 2 s here for the first ask.
    handler.postDelayed(usbWatch, UsbDongle.askAgainIn())
    handler.removeCallbacks(bandUpdate)
    handler.postDelayed(bandUpdate, 1000)
    if (!running) {
      running = true
      UsbDongle.note("Android: バックグラウンド受信を開始しました（電池の最適化 ${if (batteryRestricted(this)) "あり" else "なし"}）")
      val now = System.currentTimeMillis()
      prefs(this).edit().putLong(KEY_SINCE, now).putLong(KEY_ALIVE, now).apply()
    }
    handler.removeCallbacks(heartbeat)
    handler.postDelayed(heartbeat, 30000)
    if (!screenWatched) {
      screenWatched = true
      ContextCompat.registerReceiver(
        this,
        screen,
        IntentFilter().apply {
          addAction(Intent.ACTION_SCREEN_ON)
          addAction(Intent.ACTION_SCREEN_OFF)
        },
        ContextCompat.RECEIVER_NOT_EXPORTED,
      )
    }
    // Reception cannot resume on its own after the process is killed.
    return START_NOT_STICKY
  }

  // A hot phone throttles its CPU and then cuts USB power; past "severe",
  // the receiver pauses its second (drift-tracking) detector on every channel.
  private var thermalListener: Any? = null

  private fun watchThermal() {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q || thermalListener != null) return
    val power = getSystemService(Context.POWER_SERVICE) as PowerManager
    val listener =
      PowerManager.OnThermalStatusChangedListener { status ->
        val hot = status >= PowerManager.THERMAL_STATUS_SEVERE
        UsbDongle.note("Android: 端末の温度状態 $status${if (hot) "（処理を軽くします）" else ""}")
        try {
          NativeBand.setPowerSaving(hot)
        } catch (e: UnsatisfiedLinkError) {
          // The library is always loaded while the service runs.
        }
      }
    power.addThermalStatusListener(ContextCompat.getMainExecutor(this), listener)
    thermalListener = listener
  }

  override fun onDestroy() {
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
      (thermalListener as? PowerManager.OnThermalStatusChangedListener)?.let {
        (getSystemService(Context.POWER_SERVICE) as PowerManager).removeThermalStatusListener(it)
      }
    }
    thermalListener = null
    AudioRoute.release(this)
    handler.removeCallbacks(usbWatch)
    handler.removeCallbacks(bandUpdate)
    handler.removeCallbacks(heartbeat)
    if (screenWatched) {
      unregisterReceiver(screen)
      screenWatched = false
    }
    // Stopped normally: no killed run to report at the next start.
    running = false
    prefs(this).edit().remove(KEY_SINCE).remove(KEY_ALIVE).apply()
    UsbDongle.note("Android: バックグラウンド受信を終了しました")
    wakeLock?.let { if (it.isHeld) it.release() }
    wakeLock = null
    super.onDestroy()
  }

  private fun notification(band: NativeBand.Content?): android.app.Notification {
    val manager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
      val channel =
        NotificationChannel(CHANNEL_ID, "受信中", NotificationManager.IMPORTANCE_LOW).apply {
          description = "列車無線の受信中に表示します"
          setShowBadge(false)
        }
      manager.createNotificationChannel(channel)
    }
    val open =
      PendingIntent.getActivity(
        this,
        0,
        Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
      )
    val builder =
      NotificationCompat.Builder(this, CHANNEL_ID)
        .setSmallIcon(R.drawable.ic_stat_radio)
        .setContentTitle(band?.title ?: "列車無線を受信中")
        .setContentText(band?.text ?: "タップしてアプリを開く")
        .setContentIntent(open)
        .addAction(
          0,
          "受信を停止",
          PendingIntent.getService(
            this,
            1,
            Intent(this, ReceiverService::class.java).setAction(ACTION_STOP),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
          ),
        )
        .setOngoing(true)
        .setSilent(true)
        .setOnlyAlertOnce(true)
        .setShowWhen(false)
        .setCategory(NotificationCompat.CATEGORY_SERVICE)
        .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
    if (band != null) {
      val (small, big) = NativeBand.views(this, band)
      builder
        .setStyle(NotificationCompat.DecoratedCustomViewStyle())
        .setCustomContentView(small)
        .setCustomBigContentView(big)
    }
    return builder.build()
  }

  companion object {
    private const val CHANNEL_ID = "receiving"
    private const val NOTIFICATION_ID = 1
    private const val ACTION_STOP = "io.jrradio.receiver.STOP"
    private const val KEY_SINCE = "receiving_since"
    private const val KEY_ALIVE = "receiving_alive"

    /** True while this process runs the service. */
    @Volatile var running = false
      private set

    private fun prefs(context: Context) =
      context.getSharedPreferences("background", Context.MODE_PRIVATE)

    /** Whether Android may stop the app in the background to save battery. */
    fun batteryRestricted(context: Context): Boolean {
      val power = context.getSystemService(Context.POWER_SERVICE) as PowerManager
      return !power.isIgnoringBatteryOptimizations(context.packageName)
    }

    /**
     * A run the phone killed while receiving (the service never stopped):
     * "<start ms>\t<last sign of life ms>", or "". Read once, at the start
     * of the next process.
     */
    fun lastKill(context: Context): String {
      if (running) return ""
      val prefs = prefs(context)
      val since = prefs.getLong(KEY_SINCE, 0)
      if (since == 0L) return ""
      val alive = prefs.getLong(KEY_ALIVE, since)
      prefs.edit().remove(KEY_SINCE).remove(KEY_ALIVE).apply()
      return "$since\t$alive"
    }

    fun start(context: Context) {
      ContextCompat.startForegroundService(context, Intent(context, ReceiverService::class.java))
    }

    fun stop(context: Context) {
      context.stopService(Intent(context, ReceiverService::class.java))
    }
  }
}
