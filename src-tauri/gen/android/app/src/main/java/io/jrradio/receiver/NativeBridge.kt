package io.jrradio.receiver

/** Calls into the Rust library (libjr_receiver_desktop_lib.so, already loaded by the app). */
object NativeBridge {
  /** Headphones or Bluetooth output went away: stop the native audio monitor. */
  @JvmStatic external fun audioBecomingNoisy()
}
