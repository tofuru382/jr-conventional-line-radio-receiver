const SAMPLE_RATE = 8000;
const FRAME_SECONDS = 320 / SAMPLE_RATE;
// Cover synthesis lookahead (160 ms) plus a delayed UI poll (up to 280 ms)
// when the first half-second input burst arrives. Subsequent frames stay on
// the same audio timeline.
const BUFFER_SECONDS = 0.44;
const MAX_GAP_SECONDS = 1;
// Leave room for the initial lead, a half-second TCP burst, polling jitter,
// and gradual clock correction without prematurely dropping valid PCM.
const MAX_BUFFER_SECONDS = 1.5;

// Automatic level control for live playback (mirrors native_audio::leveler in
// src-tauri): raises quiet speakers toward a common loudness, leaves loud ones
// alone and never amplifies silence or hiss. Recordings are not affected.
// Radio transmissions are short and each may be another speaker, so speech
// after a pause is re-measured quickly instead of inheriting the last gain.
const BLOCK = 160; // 20 ms
const TARGET = 0.125; // about −18 dBFS
const GATE = 0.003; // about −50 dBFS: silence or hiss below
const MAX_GAIN = 8;
const MIN_GAIN = 0.5;
const RISE = 0.5; // per block: follow louder speech, turn down
const FALL = 0.15; // per block: follow quieter speech
const UP = 0.25; // per voiced block: turn up
const PAUSE_BLOCKS = 30; // 0.6 s without speech: next speech is new
const FRESH_BLOCKS = 10; // re-measure it quickly for 200 ms
function softLimit(x) {
  const y = x / 32767;
  const limited = y / Math.pow(1 + y ** 4, 0.25);
  return Math.max(-32768, Math.min(32767, Math.round(limited * 32767)));
}
export class Leveler {
  envelope = TARGET;
  gain = 1;
  quiet = PAUSE_BLOCKS;
  fresh = 0;
  /** Returns a new Int16-range array; the input is not modified. */
  process(pcm) {
    const out = Array.from(pcm);
    for (let start = 0; start < out.length; start += BLOCK) {
      const end = Math.min(out.length, start + BLOCK);
      let sum = 0;
      for (let i = start; i < end; i++) sum += (out[i] / 32768) ** 2;
      const rms = Math.sqrt(sum / (end - start));
      const voiced = rms > GATE;
      if (voiced) {
        if (this.quiet >= PAUSE_BLOCKS) {
          // New speech after a pause: start from unity gain (the change
          // happens in silence) and measure the speaker afresh.
          this.fresh = FRESH_BLOCKS;
          this.gain = 1;
          this.envelope = TARGET;
        }
        this.quiet = 0;
        const rate = rms > this.envelope || this.fresh > 0 ? RISE : FALL;
        this.envelope += (rms - this.envelope) * rate;
      } else this.quiet++;
      const wanted = Math.min(
        MAX_GAIN,
        Math.max(MIN_GAIN, TARGET / Math.max(this.envelope, GATE)),
      );
      // Turn down fast, turn up quickly but only while someone speaks.
      const next =
        wanted < this.gain
          ? this.gain + (wanted - this.gain) * RISE
          : voiced
            ? this.gain + (wanted - this.gain) * (this.fresh > 0 ? RISE : UP)
            : this.gain;
      if (voiced && this.fresh > 0) this.fresh--;
      const step = (next - this.gain) / (end - start);
      for (let i = start; i < end; i++)
        out[i] = softLimit(out[i] * (this.gain + step * (i - start + 1)));
      this.gain = next;
    }
    return out;
  }
}

// Schedule on the audio clock. IPC arrival times may jitter; they must never
// cancel a frame that is still playing. Reset only on an explicit stop/switch
// or a new decoder epoch.
export class AudioPlayer {
  constructor(context, destination) {
    this.context = context;
    this.destination = destination;
    this.sources = new Set();
    /** Set to a Leveler to even out the loudness of live audio. */
    this.leveler = null;
    this.reset();
  }

  get active() {
    return this.sources.size !== 0;
  }

  reset() {
    for (const node of this.sources) {
      node.onended = null;
      node.stop();
      node.disconnect();
    }
    this.sources.clear();
    this.channel = undefined;
    this.epoch = undefined;
    this.index = -1;
    this.end = undefined;
    this.rate = 1;
  }

  enqueue(frames, channel) {
    if (this.context.state !== "running") return;
    let adjusted = false;
    for (const frame of frames) {
      if (frame.channel !== channel) continue;
      if (this.channel !== channel || frame.epoch !== this.epoch) {
        // An older snapshot must not rewind the decoder's timeline.
        if (this.channel === channel && frame.epoch < this.epoch) continue;
        this.reset();
        this.channel = channel;
        this.epoch = frame.epoch;
        adjusted = false;
      }
      if (frame.index <= this.index) continue;
      const now = this.context.currentTime;
      const gap =
        this.index < 0 ? 0 : (frame.index - this.index - 1) * FRAME_SECONDS;
      if (!adjusted && gap === 0 && this.end > now) {
        // Gently track SDR/audio-device clock drift, rather than periodically
        // dropping/restarting audio. Smooth batch jitter and limit pitch change
        // to 0.5%; scheduling below uses the exact resulting playback duration.
        const correction = Math.max(
          -0.005,
          Math.min(0.005, (this.end - now - BUFFER_SECONDS) * 0.02),
        );
        this.rate += (1 + correction - this.rate) * 0.02;
      }
      adjusted = true;
      let at = this.end === undefined ? NaN : this.end + gap / this.rate;
      if (!Number.isFinite(at) || at < now || gap > MAX_GAP_SECONDS) {
        // Start/rebuffer only when input really ran out or a call resumed.
        // Keep any queued tail intact even when the next call has a new origin.
        at = Math.max(this.end ?? 0, now + BUFFER_SECONDS);
        this.rate = 1;
      }
      const end = at + frame.pcm.length / SAMPLE_RATE / this.rate;
      if (end > now + MAX_BUFFER_SECONDS) {
        // Bound exceptional catch-up bursts without stopping scheduled audio.
        // Dropped live-monitor frames must not later reappear as a timed gap.
        this.index = frame.index;
        continue;
      }
      const buffer = this.context.createBuffer(1, frame.pcm.length, SAMPLE_RATE);
      const data = buffer.getChannelData(0);
      const pcm = this.leveler ? this.leveler.process(frame.pcm) : frame.pcm;
      for (let i = 0; i < data.length; i++) data[i] = pcm[i] / 32768;
      const node = this.context.createBufferSource();
      node.buffer = buffer;
      node.playbackRate.value = this.rate;
      node.connect(this.destination);
      this.sources.add(node);
      node.onended = () => {
        this.sources.delete(node);
        node.disconnect();
      };
      node.start(at);
      this.index = frame.index;
      this.end = end;
    }
  }
}
