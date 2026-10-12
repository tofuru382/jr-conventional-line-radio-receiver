# JR Cconventional-line Radio Receiver

English | [日本語](README.ja.md)

Version: **v1.5.0**

Author: KIRISHIKI Yudai <yudai.kirishiki@protonmail.com>

A desktop receiver for digital train radio on JR conventional railway lines, built with Rust, Tauri 2, and SoapySDR.
It **demodulates 22 channels simultaneously from one wideband IQ stream and decodes each channel's voice and data independently**.
The interface is currently in Japanese; this guide includes the on-screen labels alongside their English meanings.
The product name, version, and author appear in “バージョン情報” (About) at the bottom of the window.

## Getting started

Install stable Rust, the **SoapySDR 0.8 development libraries**, the SoapySDR module for your hardware,
and the [Tauri prerequisites for your operating system](https://v2.tauri.app/start/prerequisites/).
The frontend is plain HTML, CSS, and JavaScript, so building and running the app does not require Node.js or npm.
UI development tests use Node.js 24.15 or later and `npm ci`.

Example for Ubuntu / Debian; package names vary by distribution:

```sh
sudo apt install build-essential pkg-config libsoapysdr-dev soapysdr-tools \
  libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev libssl-dev
# Add the module for your device, such as soapysdr-module-rtlsdr.
cargo run --release -p jr-receiver-desktop
```

Use `--release` for reception. The repository includes `Cargo.lock`.
GitHub Actions builds GUI and CLI binaries for Linux x86_64, Windows x64, and macOS Apple Silicon / Intel.
Manual workflow runs only generate artifacts. Pushing a tag that matches the product version, such as `v1.5.0`, also publishes a GitHub Release.
Windows archives include the SoapySDR DLL; Linux and macOS use the system's SoapySDR 0.8 installation.
See the [release guide](docs/RELEASE.md) for build instructions, runtime dependencies, CI, and publishing.
Local build and startup checks have been performed on Linux only. Installers, publisher signatures, and notarization are not provided.

## Simultaneous reception

[`ui/radio-presets.json`](ui/radio-presets.json) uses a custom JSON structure that separates channel definitions from receive profiles.
The GUI and CLI share these built-in presets.
The desktop app can also load external JSON files with the same structure, without rebuilding.
While reception is stopped, open “チャンネル一覧” (Channels) → “プリセットファイル” (Preset file) → “JSON ファイルを選択” (Select JSON file).
Use “再読み込み” (Reload) to apply edits, or “組み込みに戻す” (Restore built-in presets) to return to the three default operator profiles.

1. Choose an input source. For RTL-TCP, enter the server's **IP address and port**; both are initially empty.
   For local hardware, choose “SoapySDR デバイス” (SoapySDR device) and use “検索” (Find) to select a device.
   Advanced settings accept SoapySDR arguments, an RX channel index, and an antenna name.
2. Choose a receive profile and check the channels to receive. The default is the 22 channels allocated to JR East.
   Alternatively, select **“路線から選ぶ” (Choose by line)** above the collapsed channel list. Search by line, section endpoint, or D number, select one or more sections, review their frequencies, then apply. Reception starts only when you select “受信を開始”. See [route selection and external route metadata](docs/ROUTES.md).
   The three built-in profiles cover JR East (22 channels), JR Central (3 channels), and JR West (19 channels), with only each operator's allocated frequencies.
   The default “簡易” (Compact) view shows frequency, allocation, and synchronization status in tiles that adapt to the window width.
   “詳細” (Detailed) shows the format and voice/data counts in a table. Switching views does not affect reception, recording, the selected channel, or search filters.
3. The initial center frequency is **352.603125 MHz**, with a requested sample rate of **240,000 S/s**.
   Choose a different rate if required by your hardware. SoapySDR validates bandwidth using the sample rate and center frequency read back from the device.
   RTL-TCP does not support reading settings back, so the app displays and uses the requested values.
   Changing profiles selects the corresponding center frequency and recommended rate: **352.603125 MHz** for JR East / West,
   **352.625000 MHz** for JR Central, and **240 kS/s** for all three.
   Changing profiles does not alter the center frequency entered for IQ WAV input.
4. Select “受信を開始” (Start reception) to process all checked channels. Launching the app does not connect to a server.
   Select a frequency button to view synchronization status, EVM, the constellation, and decoded data.
5. Enable “音声を聴く” (Listen to audio) to play the selected channel. **“通話を自動選択” (Automatically select calls)** is enabled by default.
   If calls overlap, the earliest-started call takes priority. When it ends, the app selects the earliest-started call that is still active.
   **Selecting a frequency manually pins that channel.** Enable automatic selection again to resume switching.
   All other channels continue decoding and synthesizing voice. Speaker output plays one selected channel at a time.
6. Under “録音・データ保存” (Recording and data output), expand “保存内容・保存先” (Output formats and destination) to select WAV, raw-frame JSONL, or data-message JSONL.
   “保存を開始” (Start saving) saves output from every active receive channel. WAV is the only format enabled by default.
   Status and start/stop controls remain visible when the settings are collapsed.

The frequencies span 352.53750–352.66875 MHz at 6.25 kHz intervals.
This is a 131.25 kHz span. Including the demodulation filter margin, receiving all channels requires a complex sample rate greater than 139.65 kS/s.
Allow additional margin for the hardware's usable bandwidth and anti-aliasing characteristics.
If a device does not support the default 240 kS/s, choose a supported higher rate, such as 1.024 MS/s.
Hardware support depends on the available [SoapySDR drivers](https://github.com/pothosware/SoapySDR/wiki).

The overview shows the number of enabled and synchronized channels, valid data messages, voice frames, and dropped processing blocks.
Search only filters the visible list; it does not change which channels are received.
The channel list and selected-channel monitor start collapsed. Open their headers when you need channel selection, reception diagnostics, or playback settings; closing them keeps reception, audio, and data updates running. The operation monitor and received data appear side by side on wide screens, with navigation links above them. Audio on/off stays visible, and the received-data panel shows its selected frequency and a link to open the channel list. Preset errors and channel-selection validation reveal the relevant controls automatically.
“更新を一時停止” (Pause updates) freezes the selected channel’s message log while decoding and the operation monitor continue. On narrow screens, the detailed list becomes cards.

“運行把握モニタ” (Operation monitor), below the channel list, shows line-level delay counts, trains on the line, three delay classes, maximum delay, and its train number from supported JR East common-wave messages. Enable the common channel with the “一斉” format. The table updates independently of audio selection, saving, and the message-log pause. Narrow layouts show each line as a labeled record; missing values say “不明” (Unknown), rather than zero.

The operation monitor does not interpret message timestamps until the prefix format is established. It displays only the computer's last reception time in Japan time, labeled as the last read time during IQ WAV playback. This time does not indicate when the information was created or guarantee that it describes current service conditions. No age or chronological priority is inferred from the operation prefix.

Complete, validated tables replace the display in receive order. An identical prefix and decompressed table update the last reception time and repeat count; a changed prefix or table starts a new count. IQ loss retains the last complete table and its reception time. Starting a new receive session or changing the preset clears it.

The initial decoder supports the verified `0x22 / C9` common-wave form with an 11-byte prefix, an LH5 archive, and a `0x87` table containing seven 21-byte rows. RS/CRC validation and archive header/content checks are required. Other layouts are not inferred from partial data.

RTL-TCP accepts IPv4 or IPv6 addresses. Supported requested sample rates are 225001–300000 or 900001–3200000 S/s.
An empty gain field selects tuner automatic gain; a number selects manual gain. Frequency correction in ppm must be an integer.
RTL-TCP is an additional Rust TCP input and does not replace SoapySDR hardware support.
The protocol does not report server-side IQ loss or rejected settings. The “処理欠落” (Dropped processing blocks) count is the total dropped blocks in the app's worker queues;
it does not establish that the server has delivered every sample.

## Persistent settings

The desktop app saves setting changes automatically and restores them on the next launch:

- Input source, receive profile, center frequency, sample rate, and gain.
- External preset file path, or no path when using the built-in presets.
- RTL-TCP IP address, port, and frequency correction.
- SoapySDR device arguments, RX channel index, and antenna; IQ WAV file path.
- Whether saving is enabled, the output directory, and the selected WAV, raw-frame JSONL, and data-message JSONL formats.

Saved center frequencies and sample rates are not overwritten by profile defaults at startup.
Folder selections and intentionally empty fields are also retained. Startup restores settings only; reception begins when you select its start button. If saving was enabled, it resumes automatically once reception is ready.
Closing the window normally waits for pending setting writes, including recent input changes.

Settings are stored as versioned JSON in `settings.json` under Tauri's application configuration directory.
The default locations are listed below. Saving settings does not modify the source code or frequency presets.

| OS | Location |
| --- | --- |
| Linux | `$XDG_CONFIG_HOME/io.jrradio.receiver/settings.json`, or `~/.config/io.jrradio.receiver/settings.json` when unset |
| macOS | `~/Library/Application Support/io.jrradio.receiver/settings.json` |
| Windows | `%APPDATA%\io.jrradio.receiver\settings.json` |

Writes are completed in a temporary file before replacing the destination. If a write fails, “保存を再試行” (Retry saving) saves the latest input.
A corrupt file or unsupported settings version is not overwritten; the error includes its location.
Fix or move the file, then select “設定を再読み込み” (Reload settings). Changes are not saved automatically while settings restoration is failing.
The browser preview does not persist settings.

## Automatic call selection

Call order is based on the first valid voice frame, compared on the shared IQ timeline across all channels, rather than thread scheduling order.
A 120 ms decision delay accounts for synchronization acquisition; ties use preset order.
Call completion is inferred when valid voice frames stop for **one second**, rather than from volume or silent PCM.
Short reception gaps preserve priority. Calls that end while waiting are skipped. IQ loss or resynchronization resets call state.
On switching channels, playback begins with audio from within the last 600 ms; it does not replay the entire queued call.
This age limit applies to channel entry and unselected channels. While listening to the same channel, queued PCM is kept in sequence, including when RTL-TCP delivers roughly half a second of input at once. The five-second queue capacity still bounds memory use.
The voice synthesis tail is emitted even after a brief loss of signal. While a channel is manually pinned, late automatic-selection responses are ignored.

## Recording and data output

Once the receiver shows “受信中” (Receiving), select **“保存を開始” (Start saving)** under “録音・データ保存” (Recording and data output).
This enables saving for subsequent receive sessions, including after restarting the app. Stopping reception, reaching EOF, or closing the app keeps that choice. “保存を停止” (Stop saving), or “保存を無効にする” (Disable saving) while idle, disables it persistently. Existing settings without this preference default to disabled. A failed automatic start displays an error without repeatedly retrying or stopping reception.
The “保存内容・保存先” (Output formats and destination) section is collapsed initially. Expand it to choose WAV, raw-frame JSONL, data-message JSONL, and the output directory, or to view the saved session folder.
Formats can be selected independently, including JSONL-only output. Stop saving before changing them.
Status and start/stop controls remain visible. An error expands the settings, and its message remains visible if they are collapsed again.

The default output directory is `JR Cconventional-line Radio Receiver/Recordings` inside the operating system's Music folder.
The app uses the known Music folder on Windows, `~/Music` on macOS, and `XDG_MUSIC_DIR` on Linux, falling back to `~/Music` if the OS location cannot be determined.
The default directory is created when recording starts if it does not exist. You can also select or enter another existing directory.
If the default cannot be resolved or created, the recording section displays an error and lets you choose another destination.

All checked receive channels are recorded, regardless of the displayed channel, search results, monitor mute state, or playback volume.
“保存を停止” (Stop saving) leaves reception running. Stopping reception, reaching the end of IQ WAV playback, or closing the app normally also finalizes the recording.
After stopping, you can choose another folder and start again.

- Format: **8 kHz, 16-bit PCM, mono WAV**, containing voice decoded and synthesized after recording starts.
- Destination: a new `recording-YYYYMMDDTHHMMSS.sssZ-NNN/` directory under the selected folder, using UTC time and a collision-prevention counter.
  WAV filenames contain the frequency and a sequence number, such as `352537500Hz-000001.wav`. Existing files are not overwritten.
- WAV files are created only for channels that produce audio. With no audio, the audio count stays at zero; selected JSONL files and session metadata are still saved.
- Voice-frame gaps of up to five seconds are filled with silence. Longer gaps, resynchronization, or the 30-minute limit start a new file.
  Each WAV begins with its first audio; it is not a continuous wall-clock recording from the start of reception.
- `session.json` records settings, `files.jsonl` records the frequency and frame positions of finalized WAV files, and `result.json` records the completion result.
- WAV headers are finalized when files rotate or recording stops. Play recordings after stopping.
  `INCOMPLETE` remains during recording or after abnormal termination. Recovery after forced termination or power loss is not automatic.

A dedicated thread and bounded queue handle writes. If storage falls behind or a write fails, saving stops with an error while reception and monitor playback continue.
The displayed audio duration totals PCM from all channels; it excludes inserted silence and is not elapsed wall-clock time.

### JSONL output

The selected `raw-frames.jsonl` and `data-messages.jsonl` files are written in the same session directory.
They use UTF-8 with one JSON object per line. Buffers are flushed roughly every 500 ms and on stop; files are finalized on successful completion.
The UI history limit, paused display, and duplicate aggregation do not affect output. Repeated receptions of the same message are saved separately.

| File | Contents |
| --- | --- |
| `raw-frames.jsonl` | Each 48-byte radio frame extracted by the synchronizer, retained in reception order as a 96-character `raw_hex` string before descrambling or error correction. Includes frames that fail voice or data validation. Does not contain IQ samples. |
| `data-messages.jsonl` | One `message` per decoder output, including its type, identity, transmission timestamp when available, decoded fields, message `raw_hex`, and frame range. Includes voice-channel information and uninterpreted message types. Fragmented messages are emitted after assembly. |

Operation tables use `message.kind: "OperationSummary"` and an additional `message.operation_summary` object. Its `rows` contain `line`, `delayed`, `on_line`, `delay_classes` (3–9, 10–19, 20+ minutes), `max_delay_min`, `train_no`, and the original row `raw_hex`; unavailable numeric/train fields are JSON `null`. `message.timestamp` is always JSON `null` for operation tables; the outer `received_at` remains the computer's decoding time. The unknown prefix bytes are not parsed or validated as a date, and reception time is not substituted for message time. `message.raw_hex` preserves the assembled compressed body; `prefix_hex` and `expanded_hex` preserve the prefix and decompressed table. `prefix_sum_matches` records an observed byte-sum relationship whose general protocol meaning is unconfirmed; it is not an additional verified checksum guarantee.

Both formats include `schema_version: 1`, `received_at`, `channel_id`, `label`, `frequency_hz`, `format`, `epoch`, `index`, and `bit_offset`.
`received_at` is the computer's UTC time at decoding, not the transmitter's clock.
`index` is the 40 ms frame number within the channel's epoch. `bit_offset` is the bit position within the synchronizer's segment and resets after IQ loss.
Raw frames also include `sync_matched`, `marker_errors`, `voice_accepted`, and `corrected_bits`, which is numeric only for accepted voice frames.
The outer frame position of a data-message row identifies the frame that emitted the message.
Rows preserve processing order within each channel; row order across different channels is not guaranteed to follow reception timestamps.
`session.json` lists enabled formats in `outputs`. `result.json` records raw-frame and data-message counts and any error.
Write failures or output-queue overflow stop the entire saving session, leave `INCOMPLETE`, and display an error.

## IQ playback and CLI

Select “IQ WAV ファイル” (IQ WAV file) in the GUI to decode all selected channels in real time.
RIFF/WAVE stereo float32 and PCM16 are supported, with I on the left, Q on the right, and sample rates from 9.6 kS/s to 20 MS/s.
The sample rate is read from the WAV header; **enter the center frequency used when recording manually**.
RF64 and WAV files whose RIFF length wraps beyond 4 GiB are rejected. Split or convert them to standard RIFF/WAVE before loading.

```sh
# List SDR devices or print frequency presets as JSON.
cargo run --release -p jr-receiver -- devices
cargo run --release -p jr-receiver -- presets east
cargo run --release -p jr-receiver -- presets --list
cargo run --release -p jr-receiver -- presets central

# Set RTL_TCP_ENDPOINT to the IP address and port of your server.
# Receive all 22 channels for 60 seconds, print JSON statistics, then disconnect.
cargo run --release -p jr-receiver -- \
  monitor-tcp "${RTL_TCP_ENDPOINT:?Set the server IP address and port}" 60 240000 352603125 east

# Inspect a WAV header.
cargo run --release -p jr-receiver -- inspect-wave input.wav

# Decode all 22 channels to files. The final 10 limits processing to ten seconds;
# omit it to process the entire input.
cargo run --release -p jr-receiver -- \
  decode-wave input.wav 352600000 new-output east 10

# Decode saved frames.bin / indices.txt files into voice and data.
cargo run --release -p jr-receiver -- \
  decode-frames captured-stream new-decoded-output slotted
```

The output directory must not already exist. Each frequency receives `audio.wav` (8 kHz mono PCM16),
`data.jsonl` (all validated message receptions), and `messages.json` (up to 128 recent aggregated messages).
For `decode-frames`, WAV and JSONL output goes under `stream/`, with `report.json` at the top level.
No leading or trailing silence is added; gaps between voice frames are retained as silence.
Streams separated by a synchronization bit-phase change are concatenated, with epoch boundaries recorded in the report.
`INCOMPLETE` remains after failure or interruption. `report.json` includes accepted, rejected, and corrected counts and processing time for each channel.
CLI file decoding uses backpressure to prevent lost input; it does not guarantee real-time processing as live reception does.

You can also build file decoding and RTL-TCP input without SoapySDR:

```sh
cargo run --release -p jr-receiver --no-default-features -- --help
```

## Implementation and validation

- `crates/jr-conventional-dsp`: DSP for frequency translation, filtering, timing/carrier synchronization, and equalization.
- [`crates/jr-voice`](crates/jr-voice/README.md): pure Rust CELP synthesis, version 1.2.0. The decoder compares current-frame and next-frame synthesis candidates with 160 ms total lookahead. Fixed model coefficients are compiled into the executable.
- `crates/receiver-core`: synchronization, voice-channel decoding, RS / CRC, data assembly, LH5, Japanese/English text, SoapySDR / RTL-TCP, and concurrent receive workers.
- `src-tauri` / `ui`: Japanese GUI, reception controls, and Web Audio playback of the selected channel.
- `crates/receiver-cli`: device enumeration, RTL-TCP monitoring, and IQ / frame-file decoding.

IQ is acquired once and distributed through reference-counted buffers to bounded per-channel queues.
Each channel has its own DSP, synchronization, voice, and data state and runs on a separate thread.
Source overflow or queue loss invalidates continuity for the affected stream and triggers resynchronization.
IQ queues hold eight blocks per channel. The TCP input queue holds 32 blocks to absorb bursts, or up to 1.28 seconds at 240 kS/s.
TCP header reads time out after three seconds; five seconds without stream data is an error. Disconnects are distinguished from normal completion.
UI snapshots update roughly every 180 ms. Data history is limited to 128 messages per channel, and the audio monitor queue to five seconds.
Live playback starts with a 440 ms jitter buffer and schedules subsequent PCM frames continuously on the audio clock, before rendering UI details. Small arrival delays do not cancel playing audio. A gradual playback-rate adjustment (at most ±0.5%) compensates for receiver/output clock drift; the scheduled playback queue is capped at 1.5 seconds to accommodate batched input and poll jitter. These adjustments affect monitoring only, not recorded WAV samples.
Stopping joins input and processing threads. Restarting creates fresh state.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --exclude jr-receiver-desktop
cargo build --release -p jr-receiver-desktop
npm ci
npm test  # Input validation and DOM state transitions, not rendered UI tests.
node scripts/check-contrast.mjs
```
