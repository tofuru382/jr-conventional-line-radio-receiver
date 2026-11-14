package io.jrradio.receiver

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
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
  private val usbWatch =
    object : Runnable {
      override fun run() {
        if (UsbDongle.sessionWantsDevice()) UsbDongle.request(this@ReceiverService)
        handler.postDelayed(this, 2000)
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

  override fun onBind(intent: Intent?): IBinder? = null

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    val type =
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
        ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK
      } else {
        0
      }
    shown = ""
    ServiceCompat.startForeground(this, NOTIFICATION_ID, notification(null), type)
    if (wakeLock == null) {
      val power = getSystemService(Context.POWER_SERVICE) as PowerManager
      wakeLock =
        power.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "jrradio:receiver").apply {
          setReferenceCounted(false)
          acquire()
        }
    }
    handler.removeCallbacks(usbWatch)
    handler.postDelayed(usbWatch, 2000)
    handler.removeCallbacks(bandUpdate)
    handler.postDelayed(bandUpdate, 1000)
    // Reception cannot resume on its own after the process is killed.
    return START_NOT_STICKY
  }

  override fun onDestroy() {
    handler.removeCallbacks(usbWatch)
    handler.removeCallbacks(bandUpdate)
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
        .setContentText(band?.text ?: "タップしてアプリを開く・受信を停止")
        .setContentIntent(open)
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

    fun start(context: Context) {
      ContextCompat.startForegroundService(context, Intent(context, ReceiverService::class.java))
    }

    fun stop(context: Context) {
      context.stopService(Intent(context, ReceiverService::class.java))
    }
  }
}
