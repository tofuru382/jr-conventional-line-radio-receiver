package io.jrradio.receiver

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ServiceInfo
import android.media.AudioManager
import android.os.Build
import android.os.IBinder
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

  // Headphones unplugged or Bluetooth disconnected: Android would route the
  // monitor to the loudspeaker. Silence it instead (e.g. on a crowded train).
  private val becomingNoisy =
    object : BroadcastReceiver() {
      override fun onReceive(context: Context, intent: Intent) {
        if (intent.action == AudioManager.ACTION_AUDIO_BECOMING_NOISY) {
          try {
            NativeBridge.audioBecomingNoisy()
          } catch (e: UnsatisfiedLinkError) {
            // Library not loaded yet: nothing is playing.
          }
        }
      }
    }

  override fun onBind(intent: Intent?): IBinder? = null

  override fun onCreate() {
    super.onCreate()
    ContextCompat.registerReceiver(
      this,
      becomingNoisy,
      IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY),
      ContextCompat.RECEIVER_NOT_EXPORTED,
    )
  }

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    val type =
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
        ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK
      } else {
        0
      }
    ServiceCompat.startForeground(this, NOTIFICATION_ID, notification(), type)
    if (wakeLock == null) {
      val power = getSystemService(Context.POWER_SERVICE) as PowerManager
      wakeLock =
        power.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "jrradio:receiver").apply {
          setReferenceCounted(false)
          acquire()
        }
    }
    // Reception cannot resume on its own after the process is killed.
    return START_NOT_STICKY
  }

  override fun onDestroy() {
    unregisterReceiver(becomingNoisy)
    wakeLock?.let { if (it.isHeld) it.release() }
    wakeLock = null
    super.onDestroy()
  }

  private fun notification(): android.app.Notification {
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
    return NotificationCompat.Builder(this, CHANNEL_ID)
      .setSmallIcon(R.drawable.ic_stat_radio)
      .setContentTitle("列車無線を受信中")
      .setContentText("タップしてアプリを開く・受信を停止")
      .setContentIntent(open)
      .setOngoing(true)
      .setSilent(true)
      .setCategory(NotificationCompat.CATEGORY_SERVICE)
      .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
      .build()
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
