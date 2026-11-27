package io.jrradio.receiver

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.media.session.MediaSession
import android.media.session.PlaybackState
import androidx.core.content.ContextCompat

/**
 * Where the monitor audio may go, for ui/app.js (which plays it through Web
 * Audio and asks through [MainActivity.Bridge.audioRoute]):
 *
 * - Headphones unplugged or Bluetooth gone ("becoming noisy"): Android would
 *   switch to the loudspeaker, so the page mutes instead (e.g. on a train).
 * - Audio focus: a phone call pauses listening until it ends; another app
 *   taking the audio for good mutes it.
 * - A media session, so headset buttons and the lock screen can mute and
 *   unmute.
 *
 * The page reads counters rather than callbacks: its scripts may run late in
 * the background, and a counter cannot miss an event.
 */
object AudioRoute {
  @Volatile private var noisy = 0
  @Volatile private var toggles = 0
  @Volatile private var focus = "none"

  private var listening = false
  private var request: AudioFocusRequest? = null
  private var session: MediaSession? = null
  private var receiverRegistered = false

  private val becomingNoisy =
    object : BroadcastReceiver() {
      override fun onReceive(context: Context, intent: Intent) {
        if (intent.action == AudioManager.ACTION_AUDIO_BECOMING_NOISY) {
          noisy += 1
          UsbDongle.note("Android: イヤホンなどの音声出力が外れました（消音します）")
        }
      }
    }

  private val focusChange =
    AudioManager.OnAudioFocusChangeListener { change ->
      focus =
        when (change) {
          AudioManager.AUDIOFOCUS_GAIN -> "held"
          AudioManager.AUDIOFOCUS_LOSS -> "lost"
          AudioManager.AUDIOFOCUS_LOSS_TRANSIENT -> "transient"
          // Ducking: notifications and navigation prompts; keep playing.
          AudioManager.AUDIOFOCUS_LOSS_TRANSIENT_CAN_DUCK -> "held"
          else -> focus
        }
      UsbDongle.note("Android: 音声フォーカス $focus")
    }

  /** Starts watching for unplugged outputs (while the service runs). */
  @Synchronized
  fun watch(context: Context) {
    if (receiverRegistered) return
    receiverRegistered = true
    ContextCompat.registerReceiver(
      context.applicationContext,
      becomingNoisy,
      IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY),
      ContextCompat.RECEIVER_NOT_EXPORTED,
    )
  }

  /** Called by the page when "listen" changes while receiving. */
  @Synchronized
  fun setListening(context: Context, on: Boolean) {
    if (on == listening) return
    listening = on
    val audio = context.getSystemService(Context.AUDIO_SERVICE) as AudioManager
    // minSdk is 26: AudioFocusRequest is always available.
    if (on) {
      val attributes =
        AudioAttributes.Builder()
          .setUsage(AudioAttributes.USAGE_MEDIA)
          .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
          .build()
      val next =
        AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
          .setAudioAttributes(attributes)
          .setOnAudioFocusChangeListener(focusChange)
          .setWillPauseWhenDucked(false)
          .build()
      request = next
      focus =
        if (audio.requestAudioFocus(next) == AudioManager.AUDIOFOCUS_REQUEST_GRANTED) "held"
        else "transient"
    } else {
      request?.let { audio.abandonAudioFocusRequest(it) }
      request = null
      focus = "none"
    }
    mediaSession(context).apply {
      setPlaybackState(
        PlaybackState.Builder()
          .setActions(PlaybackState.ACTION_PLAY or PlaybackState.ACTION_PAUSE or PlaybackState.ACTION_PLAY_PAUSE)
          .setState(if (on) PlaybackState.STATE_PLAYING else PlaybackState.STATE_PAUSED, 0, 1f)
          .build(),
      )
      isActive = true
    }
  }

  /**
   * The page's own recording player took the focus (it plays in the same
   * WebView, which Android treats like another app): take it back once that
   * playback stopped, so another app taking it later still mutes.
   */
  @Synchronized
  fun reclaim(context: Context) {
    val held = request ?: return
    if (!listening || focus == "held") return
    val audio = context.getSystemService(Context.AUDIO_SERVICE) as AudioManager
    focus =
      if (audio.requestAudioFocus(held) == AudioManager.AUDIOFOCUS_REQUEST_GRANTED) "held"
      else "transient"
    UsbDongle.note("Android: 録音の再生が終わったので音声フォーカスを取り直しました（$focus）")
  }

  private fun mediaSession(context: Context): MediaSession =
    session
      ?: MediaSession(context.applicationContext, "jrradio-monitor").also { created ->
        created.setCallback(
          object : MediaSession.Callback() {
            override fun onPlay() {
              if (!listening) toggles += 1
            }

            override fun onPause() {
              if (listening) toggles += 1
            }
          },
        )
        session = created
      }

  /** Reception ended: give the audio back and hide the media session. */
  @Synchronized
  fun release(context: Context) {
    setListening(context, false)
    session?.isActive = false
  }

  /** {"noisy":N,"focus":"held|lost|transient|none","toggles":N} */
  fun state(): String = """{"noisy":$noisy,"focus":"$focus","toggles":$toggles}"""
}
