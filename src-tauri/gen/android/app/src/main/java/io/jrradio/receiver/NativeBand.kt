package io.jrradio.receiver

import android.content.Context
import android.content.res.Configuration
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.RectF
import android.widget.RemoteViews
import java.util.Locale
import org.json.JSONObject
import kotlin.math.roundToInt

/**
 * The band map in the receiving notification, as on the app's screen: one
 * cell per channel of the preset at its frequency, green while synchronised,
 * amber and taller during a call, red on an error, grey while searching and
 * outlined when not received; the one being heard is underlined.
 *
 * The page sends the layout (every channel, and which are received) when it
 * changes; the live state comes from Rust (src-tauri/src/band.rs) once a
 * second, so the notification keeps updating with the app in the background.
 */
object NativeBand {
  private const val STEP_HZ = 6250.0

  /** Implemented in Rust: `{"state", "heard", "channels": {id: state}}`. */
  @JvmStatic external fun state(): String

  /** Implemented in Rust: stops reception and saving (notification button). */
  @JvmStatic external fun stop()

  /** `{"channels": [{"id", "hz", "label"}], "enabled": [id]}` from ui/app.js. */
  @Volatile var layout: String? = null

  class Content(
    val title: String,
    val text: String,
    val low: String,
    val high: String,
    /** Cells left to right: (slot, state, heard). */
    val cells: List<Triple<Int, String, Boolean>>,
    val slots: Int,
  ) {
    /** Equal content needs no new notification. */
    val signature: String = "$title|$text|${cells.joinToString(",") { "${it.first}${it.second}${it.third}" }}"
  }

  private class Channel(val id: String, val hz: Double, val label: String)

  /** The current content, or null without a layout or a session. */
  fun content(): Content? {
    val layoutJson = layout ?: return null
    val stateJson =
      try {
        state()
      } catch (e: UnsatisfiedLinkError) {
        return null
      }
    return try {
      build(JSONObject(layoutJson), JSONObject(stateJson))
    } catch (e: Exception) {
      null
    }
  }

  private fun build(layout: JSONObject, live: JSONObject): Content? {
    val session = live.optString("state")
    if (session == "stopped") return null
    val list = layout.getJSONArray("channels")
    val channels =
      (0 until list.length())
        .map { list.getJSONObject(it) }
        .map { Channel(it.getString("id"), it.getDouble("hz"), it.optString("label")) }
        .sortedBy { it.hz }
    if (channels.isEmpty()) return null
    val enabledList = layout.optJSONArray("enabled")
    val enabled = (0 until (enabledList?.length() ?: 0)).map { enabledList!!.getString(it) }.toSet()
    val states = live.optJSONObject("channels") ?: JSONObject()
    val heard = if (live.isNull("heard")) null else live.optString("heard")
    val low = channels.first().hz
    val slot = { hz: Double -> ((hz - low) / STEP_HZ).roundToInt() }
    val cells =
      channels.map { c ->
        val state = if (c.id !in enabled) "untargeted" else states.optString(c.id, "idle")
        Triple(slot(c.hz), state, c.id == heard)
      }
    val synced = enabled.count { states.optString(it) == "locked" || states.optString(it) == "calling" }
    val calls = enabled.count { states.optString(it) == "calling" }
    val heardChannel = channels.firstOrNull { it.id == heard }
    val heardState =
      when (heardChannel?.let { states.optString(it.id, "idle") }) {
        "calling" -> "通話中"
        "locked" -> "同期中"
        "error" -> "エラー"
        null -> null
        else -> "探索中"
      }
    val title =
      when {
        session == "reconnecting" -> "再接続中"
        session == "starting" -> "準備中"
        heardChannel != null -> "${heardChannel.label}　$heardState"
        else -> "列車無線を受信中"
      }
    val text =
      listOfNotNull(
          heardChannel?.let { String.format(Locale.ROOT, "%.5f MHz", it.hz / 1e6) },
          "同期 $synced / ${enabled.size} 波",
          if (calls > 0) "通話 $calls" else null,
        )
        .joinToString("　")
    return Content(
      title,
      text,
      String.format(Locale.ROOT, "%.5f", low / 1e6),
      String.format(Locale.ROOT, "%.5f", channels.last().hz / 1e6),
      cells,
      slot(channels.last().hz) + 1,
    )
  }

  private class Palette(
    val locked: Int,
    val calling: Int,
    val error: Int,
    val idle: Int,
    val heard: Int,
  )

  private fun palette(context: Context): Palette {
    val night =
      context.resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK ==
        Configuration.UI_MODE_NIGHT_YES
    return if (night) {
      Palette(0xff6ccb5f.toInt(), 0xfffce100.toInt(), 0xffff99a4.toInt(), 0xff5d5d5d.toInt(), 0xff60cdff.toInt())
    } else {
      Palette(0xff107c10.toInt(), 0xff9d5d00.toInt(), 0xffc42b1c.toInt(), 0xff8a8a8a.toInt(), 0xff0078d7.toInt())
    }
  }

  /** The band drawn at `heightDp` (the views stretch it to their width). */
  fun bitmap(context: Context, content: Content, heightDp: Int): Bitmap {
    val density = context.resources.displayMetrics.density
    val width = (320 * density).roundToInt()
    val height = (heightDp * density).roundToInt()
    val bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
    val canvas = Canvas(bitmap)
    val colors = palette(context)
    val paint = Paint(Paint.ANTI_ALIAS_FLAG)
    val gap = 2 * density
    val underline = 3 * density
    val area = height - underline - density
    val cell = (width - gap * (content.slots - 1)) / content.slots
    val radius = 0f
    for ((slot, state, heard) in content.cells) {
      val left = slot * (cell + gap)
      val right = left + cell
      val top = if (state == "calling") area * 0.08f else area * 0.5f
      val rect = RectF(left, top, right, area)
      when (state) {
        "untargeted" -> {
          paint.style = Paint.Style.STROKE
          paint.strokeWidth = density
          paint.color = colors.idle
          rect.inset(density / 2, density / 2)
          canvas.drawRoundRect(rect, radius, radius, paint)
        }
        else -> {
          paint.style = Paint.Style.FILL
          paint.color =
            when (state) {
              "locked" -> colors.locked
              "calling" -> colors.calling
              "error" -> colors.error
              else -> colors.idle
            }
          canvas.drawRoundRect(rect, radius, radius, paint)
        }
      }
      if (heard) {
        paint.style = Paint.Style.FILL
        paint.color = colors.heard
        canvas.drawRoundRect(RectF(left - gap / 2, height - underline, right + gap / 2, height.toFloat()), radius, radius, paint)
      }
    }
    return bitmap
  }

  /** Collapsed and expanded views for the notification. */
  fun views(context: Context, content: Content): Pair<RemoteViews, RemoteViews> {
    val small = RemoteViews(context.packageName, R.layout.notification_band_small)
    small.setTextViewText(R.id.band_title, "${content.title}　${content.text}")
    small.setImageViewBitmap(R.id.band_map, bitmap(context, content, 16))
    val big = RemoteViews(context.packageName, R.layout.notification_band)
    big.setTextViewText(R.id.band_title, content.title)
    big.setTextViewText(R.id.band_text, content.text)
    big.setImageViewBitmap(R.id.band_map, bitmap(context, content, 30))
    big.setTextViewText(R.id.band_low, content.low)
    big.setTextViewText(R.id.band_high, content.high)
    return small to big
  }
}
