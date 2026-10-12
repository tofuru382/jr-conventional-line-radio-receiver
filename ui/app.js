import { makeInput } from "./config.mjs";
import { buildCatalog } from "./presets.mjs";
import { AudioPlayer } from "./audio.mjs";
import { OperationMonitorView } from "./operation-monitor.mjs";
const operationMonitor = new OperationMonitorView(document);
let sessionReplay = false;
const $ = (id) => document.getElementById(id);
const native = !!window.__TAURI__;
const invoke = (command, args) => window.__TAURI__.core.invoke(command, args);
// Desktop unless the backend reports otherwise. Android has no SoapySDR or
// folder picker and receives from an on-device RTL-TCP driver app instead.
let platform = { android: false, soapy: true };
const androidDriver = { host: "127.0.0.1", port: "1234" };
// Android plays the monitor natively (it keeps playing in the background) and
// keeps a foreground service running while receiving.
let nativeAudio = false,
  nativeMonitorState = "",
  serviceReceiving = false,
  outputLostSeen = 0,
  audioNotice = "";
// The native player silences itself when headphones/Bluetooth disconnect;
// reflect that by turning "listen" off until the user turns it back on.
function handleOutputLost(events) {
  if (!nativeAudio || !(events > outputLostSeen)) return;
  outputLostSeen = events;
  if (!$("listen").checked) return;
  $("listen").checked = false;
  audioNotice = "イヤホンの接続が切れたため消音しました";
  syncNativeMonitor();
}
function syncNativeMonitor() {
  if (!nativeAudio) return;
  const settings = {
    enabled: running && $("listen").checked,
    selected: selected || null,
    automatic: $("auto-select").checked,
    volume: Number($("volume").value) / 100,
  };
  const next = JSON.stringify(settings);
  if (next === nativeMonitorState) return;
  nativeMonitorState = next;
  invoke("set_native_monitor", { settings }).catch((e) => {
    nativeMonitorState = "";
    fail(e);
  });
}
function syncBackgroundService() {
  if (!platform.android || serviceReceiving === running) return;
  serviceReceiving = running;
  try {
    window.JrAndroid?.setReceiving(running);
  } catch {
    // The bridge only exists in the Android app.
  }
}
let channels = [],
  enabled = new Set(),
  selected = "",
  running = false,
  busy = false,
  lastSnapshot = null;
let sessionVisible = false,
  uiGeneration = 0;
let messagesPaused = false;
let selectionRevision = 0;
let preferencesReady = false,
  preferencesLoadFailed = false,
  preferencesTask = null,
  desiredPreferences = null,
  savedPreferences = "";
const preferenceFields = {
  kind: "input-kind",
  profile: "region",
  center: "center",
  rate: "rate",
  gain: "gain",
  host: "host",
  port: "port",
  ppm: "ppm",
  args: "args",
  rx_channel: "rx-channel",
  antenna: "antenna",
  path: "path",
};
const emptyRecording = () => ({
  state: "idle",
  files: 0,
  pcm_samples: 0,
  error: null,
});
let recordingStatus = emptyRecording(),
  recordingActionError = "",
  recordingEnabled = false,
  recordingResumePending = false;
let bundledCatalog = [],
  presetCatalog = [],
  presetFile = null,
  activePresetId = "east";
let context, gain, audioPlayer;
const rows = new Map(),
  messageNodes = new Map();
const kinds = {
  ServiceInformation: "旅客一斉情報",
  OperationSummary: "運行把握",
  MessageDirectory: "情報目録",
  Registration: "列車登録",
  Presence: "圏内識別",
  Transfer: "情報転送",
  ChannelActivity: "音声チャンネル情報",
  Notice: "通告",
  CallSetup: "呼設定",
  Unsupported: "未解釈のメッセージ",
};
const fieldNames = {
  Line: "路線",
  LineCount: "線区数",
  From: "発 / 区間始点",
  To: "着 / 区間終点",
  Direction: "方向",
  Cause: "原因",
  ServiceStatus: "運行状況",
  Description: "詳細",
  RelatedMessage: "関連情報",
  DestinationCodes: "宛先コード",
  MessageIds: "情報番号",
  ClassCode: "形式コード",
  Formation: "編成",
  TrainNumber: "列車番号",
  NumberPhase: "番号設定段階",
  CabFlags: "運転台フラグ",
  DispatchNotice: "指令通知",
  NumberPrefix: "番号接頭辞",
  VehicleKind: "車種",
  VehicleSeries: "系列",
  VehicleNumber: "車両番号",
  LineCode: "路線コード",
  StationCode: "駅コード",
  RadioNumber: "無線番号",
  Subtype: "サブタイプ",
  ToAll: "一斉",
  Serial: "通番",
  RecordKind: "記録種別",
  DataKind: "データ種別",
  Channel: "チャンネル",
  Frequency: "周波数 Hz",
  ChannelStatus: "状態コード",
  NumberState: "番号状態",
  CabRole: "運転台役割",
  ControlIdentifier: "制御識別子",
  TargetMode: "対象",
  Layout: "形式",
  Category: "分類コード",
  NoticeNumber: "通告番号",
  IssuedAtBytes: "発行日時 bytes",
  TextBytes: "本文 bytes",
  UnassignedBytes: "未解釈 bytes",
  CallAddress: "呼出アドレス",
  CallCabCode: "運転台コード",
  CallResultCode: "呼設定結果",
  CallSetupStep: "設定段階",
  FromStationCode: "発駅コード",
  ToStationCode: "着駅コード",
  NoticeText: "通告本文",
};
function el(tag, text, className) {
  const n = document.createElement(tag);
  if (text !== undefined) n.textContent = text;
  if (className) n.className = className;
  return n;
}
$("about-open").addEventListener("click", () => {
  const dialog = $("about-dialog");
  if (!dialog.open) dialog.showModal();
});
$("about-close").addEventListener("click", () => $("about-dialog").close());
$("about-dialog").addEventListener("close", () => $("about-open").focus());
// Navigation into a disclosure must reveal its content before moving focus.
document.addEventListener("click", (event) => {
  const link = event.target.closest?.('a[href^="#"]');
  if (!link || event.ctrlKey || event.metaKey || event.shiftKey || event.altKey)
    return;
  const target = document.getElementById(link.hash.slice(1));
  if (!target) return;
  const panel = target.closest(".panel-disclosure");
  if (panel) panel.open = true;
  target.focus({ preventScroll: true });
});
function fail(error) {
  const text = `${error.message ?? error}\n設定と接続を確認し、受信を再開してください。`;
  if ($("error").textContent !== text) $("error").textContent = text;
  $("error").hidden = false;
}
function clearError() {
  $("error").hidden = true;
  $("field-error").hidden = true;
  for (const field of document.querySelectorAll('[aria-invalid="true"]')) {
    field.removeAttribute("aria-invalid");
    field.removeAttribute("aria-describedby");
  }
}
function preferenceError(error, loading = false) {
  $("preferences-message").textContent =
    `${loading ? "設定を復元できませんでした。" : "変更した設定を保存できませんでした。"}${error.message ?? error}`;
  $("preferences-error").hidden = false;
  $("preferences-retry").textContent = loading
    ? "設定を再読み込み"
    : "保存を再試行";
}
function capturePreferences() {
  return {
    version: 1,
    preset_file: presetFile,
    input: Object.fromEntries(
      Object.entries(preferenceFields).map(([key, id]) => [key, $(id).value]),
    ),
    output: {
      enabled: recordingEnabled,
      directory: $("record-directory").value,
      ...outputOptions(),
    },
  };
}
function persistPreferences() {
  if (!native || !preferencesReady) return Promise.resolve();
  desiredPreferences = capturePreferences();
  if (preferencesTask) return preferencesTask;
  let failed = false;
  preferencesTask = (async () => {
    while (JSON.stringify(desiredPreferences) !== savedPreferences) {
      const next = desiredPreferences;
      try {
        await invoke("save_preferences", { settings: next });
        savedPreferences = JSON.stringify(next);
        $("preferences-error").hidden = true;
      } catch (error) {
        failed = true;
        preferenceError(error);
        break;
      }
    }
  })().finally(() => {
    preferencesTask = null;
    if (!failed && JSON.stringify(desiredPreferences) !== savedPreferences)
      void persistPreferences();
  });
  return preferencesTask;
}
async function flushPreferences() {
  await persistPreferences();
  while (preferencesTask) await preferencesTask;
}
async function restorePreferences(initial = false) {
  preferencesReady = false;
  preferencesLoadFailed = false;
  let saved = null;
  if (native) {
    try {
      saved = await invoke("load_preferences");
    } catch (error) {
      preferencesLoadFailed = true;
      preferenceError(error, true);
    }
  }
  recordingEnabled = saved?.output.enabled === true;
  if (saved) {
    for (const [key, id] of Object.entries(preferenceFields))
      if (key !== "profile") $(id).value = saved.input[key];
    const args = $("args").value;
    if (args) {
      const option = el("option", "保存済みのデバイス");
      option.value = args;
      $("device").replaceChildren(option);
    }
    $("output-audio").checked = saved.output.audio;
    $("output-raw").checked = saved.output.raw_frames;
    $("output-data").checked = saved.output.data_messages;
  }
  presetFile = saved?.preset_file ?? null;
  const preferred = saved?.input.profile ?? activePresetId;
  try {
    if (presetFile)
      $("preset-status").textContent = "プリセットファイルを読み込み中…";
    const catalog = presetFile
      ? await invoke("preset_catalog", { path: presetFile })
      : bundledCatalog;
    const retained = installCatalog(catalog, preferred);
    await loadPresets((initial && !saved) || !retained);
    $("preset-error").hidden = true;
  } catch (error) {
    // Do not silently receive a different catalog when the saved file is gone.
    presetCatalog = [];
    channels = [];
    enabled.clear();
    sessionVisible = false;
    lastSnapshot = null;
    uiGeneration++;
    activePresetId = preferred;
    const option = el("option", "プリセットを読み込めません");
    option.value = preferred;
    $("region").replaceChildren(option);
    buildRows();
    $("band-low").textContent = $("band-high").textContent = "—";
    $("band-note").textContent = "プリセット未読込";
    $("all-label").textContent = "受信チャンネル未読込";
    $("preset-note").textContent = "プリセットファイルを確認してください。";
    showPresetError(error);
  }
  renderPresetSource();
  if (
    saved?.output.directory !== null &&
    saved?.output.directory !== undefined
  ) {
    $("record-directory").value = saved.output.directory;
    clearRecordingError();
  } else if (native && (initial || !$("record-directory").value)) {
    try {
      $("record-directory").value = await invoke("default_recording_directory");
    } catch (error) {
      recordingActionError = String(error.message ?? error);
    }
  }
  preferencesReady = native && !preferencesLoadFailed;
  const restored = capturePreferences();
  if (saved) restored.input.profile = saved.input.profile;
  savedPreferences = saved ? JSON.stringify(restored) : "";
  if (!preferencesLoadFailed) $("preferences-error").hidden = true;
  setControls();
}
$("preferences-retry").addEventListener("click", async () => {
  if (preferencesLoadFailed) {
    if (running || busy) return;
    busy = true;
    setControls();
    try {
      await restorePreferences();
    } catch (error) {
      preferencesLoadFailed = true;
      preferenceError(error, true);
    } finally {
      busy = false;
      setControls();
    }
  } else await flushPreferences();
});
function setControls() {
  $("settings").disabled = running || busy;
  $("region").disabled = running || busy || !channels.length;
  $("preset-browse").disabled = !native || running || busy;
  $("preset-reload").disabled = !native || running || busy || !presetFile;
  $("preset-builtin").disabled = !native || running || busy || !presetFile;
  $("all").disabled = running || busy || !channels.length;
  $("start").disabled = !native || running || busy || !channels.length;
  $("stop").disabled = !running || busy;
  $("selected-format").disabled = running || busy || !channels.length;
  $("preferences-retry").disabled = busy || (preferencesLoadFailed && running);
  $("total-enabled").textContent = enabled.size;
  $("audio-state").textContent = !$("listen").checked
    ? audioNotice || "消音中"
    : nativeAudio
      ? "音声オン"
      : audioPlayer?.active
      ? "音声再生中"
      : "音声待機";
  for (const [id, r] of rows) {
    for (const check of [r.check, r.simple.check]) {
      check.disabled = running || busy;
      check.checked = enabled.has(id);
    }
  }
  renderSelectionMode();
  renderChannelStates();
  $("channel-count").textContent =
    `${presetCatalog.find((p) => p.id === activePresetId)?.label ?? "プリセット未読込"} · ${enabled.size} 波を選択 · ${running ? "同時処理中" : "待機"}`;
  $("all").checked = enabled.size === channels.length;
  $("all").indeterminate = enabled.size > 0 && enabled.size < channels.length;
  renderRecording();
  operationMonitor.update(lastSnapshot, { channels, enabled, running, replay: sessionReplay });
}
function renderSelectionMode() {
  const automatic = $("auto-select").checked;
  const hint = automatic
    ? "先に始まった通話を優先します。手動で選ぶと固定します。"
    : "手動選択を固定中。自動切替に戻すにはチェックを入れてください。";
  if ($("selection-hint").textContent !== hint)
    $("selection-hint").textContent = hint;
}
$("auto-select").addEventListener("change", () => {
  selectionRevision++;
  renderSelectionMode();
  syncNativeMonitor();
});
function outputOptions() {
  return {
    audio: $("output-audio").checked,
    raw_frames: $("output-raw").checked,
    data_messages: $("output-data").checked,
  };
}
for (const id of ["output-audio", "output-raw", "output-data"]) {
  $(id).addEventListener("change", () => {
    clearRecordingError();
    renderRecording();
  });
}
function renderRecording() {
  const s = recordingStatus;
  const active = s.state === "recording" || s.state === "stopping";
  const receiving = running && lastSnapshot?.state === "receiving";
  $("record-directory").disabled = busy || active;
  $("output-options").disabled = busy || active;
  $("record-browse").disabled = !native || busy || active;
  $("record-start").disabled = !native || busy || !receiving || active;
  $("record-stop").disabled = !native || busy || (!active && !recordingEnabled);
  $("record-stop").textContent = active ? "保存を停止" : "保存を無効にする";
  const label =
    {
      idle: recordingEnabled ? "保存待機" : "未保存",
      recording: "保存中",
      stopping: "保存を終了中",
      completed: "保存完了",
      failed: "保存エラー",
    }[s.state] ?? s.state;
  if ($("recording-state").textContent !== label)
    $("recording-state").textContent = label;
  $("recording-state").classList.toggle("active", active);
  const idleMessage = recordingEnabled
    ? "受信開始後に自動で保存します。"
    : receiving
      ? "保存を開始できます。"
      : "受信を開始してから保存できます。";
  const summary =
    s.state === "idle"
      ? `${idleMessage}音声は 8 kHz・16 bit・モノラル。`
      : `WAV ${s.files} ファイル · 復号音声 ${(s.pcm_samples / 8000).toFixed(1)} 秒 · 生フレーム ${s.raw_frames ?? 0} 件 · データ電文 ${s.data_messages ?? 0} 件（全波合計）。受信停止時にも保存を終了します。`;
  $("recording-summary").textContent =
    summary +
    (recordingEnabled && s.state !== "idle"
      ? "次回受信時も自動で保存します。"
      : "");
  if (s.directory) {
    $("recording-output").textContent =
      `${active ? "保存フォルダー" : "保存したフォルダー"}：${s.directory}`;
    $("recording-output").hidden = false;
  }
  const error = recordingActionError || s.error;
  const message = error
    ? `${error} 設定を確認して、保存を再開してください。`
    : "";
  if ($("recording-error").textContent !== message) {
    $("recording-error").textContent = message;
    if (error) $("recording-settings").open = true;
  }
  $("recording-error").hidden = !error;
}
function clearRecordingError() {
  recordingActionError = "";
  $("record-directory").removeAttribute("aria-invalid");
  $("record-directory").setAttribute("aria-describedby", "recording-hint");
}
$("record-directory").addEventListener("input", () => {
  clearRecordingError();
  renderRecording();
});
$("record-browse").addEventListener("click", async () => {
  try {
    const path = await invoke("plugin:dialog|open", {
      options: {
        multiple: false,
        directory: true,
        title: "録音・データの保存先フォルダーを選択",
      },
    });
    if (typeof path === "string") {
      $("record-directory").value = path;
      clearRecordingError();
      void persistPreferences();
    }
  } catch (e) {
    recordingActionError = String(e.message ?? e);
  }
  renderRecording();
});
async function startRecording(automatic = false) {
  recordingResumePending = false;
  clearRecordingError();
  const directory = $("record-directory").value.trim();
  if (!directory) {
    $("recording-settings").open = true;
    recordingActionError = "保存先フォルダーを選択してください。";
    $("record-directory").setAttribute("aria-invalid", "true");
    $("record-directory").setAttribute("aria-describedby", "recording-error");
    renderRecording();
    $("record-directory").focus();
    return;
  }
  const options = outputOptions();
  if (!Object.values(options).some(Boolean)) {
    recordingActionError = "保存する内容を 1 つ以上選択してください。";
    renderRecording();
    $("output-audio").focus();
    return;
  }
  try {
    uiGeneration++;
    busy = true;
    setControls();
    await flushPreferences();
    recordingStatus = await invoke("start_recording", { directory, options });
    recordingEnabled = true;
    await flushPreferences();
  } catch (e) {
    recordingActionError = String(e.message ?? e);
  } finally {
    busy = false;
    setControls();
    if (!automatic && !$("record-stop").disabled) $("record-stop").focus();
  }
}
$("record-start").addEventListener("click", () => startRecording());
$("record-stop").addEventListener("click", async () => {
  try {
    uiGeneration++;
    busy = true;
    recordingEnabled = false;
    recordingResumePending = false;
    const active = ["recording", "stopping"].includes(recordingStatus.state);
    if (active) recordingStatus = { ...recordingStatus, state: "stopping" };
    setControls();
    await flushPreferences();
    if (active) recordingStatus = await invoke("stop_recording");
  } catch (e) {
    recordingActionError = String(e.message ?? e);
  } finally {
    busy = false;
    setControls();
    if (!$("record-start").disabled) $("record-start").focus();
  }
});
function resetAudio() {
  audioPlayer?.reset();
}
async function prepareAudio() {
  if (nativeAudio || !$("listen").checked) return;
  if (!context) {
    context = new AudioContext();
    gain = context.createGain();
    gain.connect(context.destination);
    audioPlayer = new AudioPlayer(context, gain);
  }
  gain.gain.value = Number($("volume").value) / 100;
  await context.resume();
}
function play(frames) {
  if (nativeAudio) return;
  if (!context || context.state !== "running" || !$("listen").checked) return;
  audioPlayer.enqueue(frames, selected);
}
function receiveCheck(c) {
  const check = el("input");
  check.type = "checkbox";
  check.checked = enabled.has(c.id);
  check.setAttribute(
    "aria-label",
    `${(c.frequency_hz / 1e6).toFixed(5)} MHz を受信`,
  );
  check.addEventListener("change", () => {
    check.checked ? enabled.add(c.id) : enabled.delete(c.id);
    setControls();
  });
  const label = el("label", undefined, "row-check");
  label.append(check);
  return { check, label };
}
// Known lines using the channel's group (reference examples, not exhaustive).
function channelLines(id) {
  return (
    presetCatalog.find((p) => p.id === activePresetId)?.channel_lines?.[id] ??
    ""
  );
}
function channelButton(c, simple = false) {
  const button = el("button", undefined, "channel-select");
  button.type = "button";
  const identity = el("span", undefined, "channel-identity");
  const frequency = el("span", undefined, "frequency");
  frequency.append(
    el("bdi", (c.frequency_hz / 1e6).toFixed(5)),
    el("span", " MHz", simple ? "frequency-unit" : "sr-only"),
  );
  identity.append(frequency, el("span", c.label, "channel-label"));
  const lines = channelLines(c.id);
  if (lines) {
    const node = el("span", lines, "channel-lines");
    node.title = `使用例：${lines}`;
    identity.append(node);
  }
  button.append(identity);
  button.addEventListener("click", () => select(c.id, true));
  return button;
}
function buildRows() {
  rows.clear();
  $("channels").replaceChildren();
  $("channels-simple").replaceChildren();
  for (const c of channels) {
    const tr = el("tr");
    const box = el("td");
    const { check, label: checkLabel } = receiveCheck(c);
    box.append(checkLabel);
    const name = el("td");
    const button = channelButton(c);
    name.append(button);
    const mode = el("td");
    const format = el(
      "span",
      c.format === "common" ? "一斉" : "通常",
      "format-label",
    );
    mode.append(format);
    const sync = el("td", "待機", "lost"),
      voice = el("td", "—", "numeric"),
      data = el("td", "—", "numeric");
    mode.dataset.label = "形式";
    sync.dataset.label = "同期";
    voice.dataset.label = "音声";
    data.dataset.label = "データ";
    tr.append(box, name, mode, sync, voice, data);
    $("channels").append(tr);
    const item = el("li", undefined, "channel-tile");
    const simpleCheck = receiveCheck(c);
    const simpleButton = channelButton(c, true);
    const simpleSync = el("span", "待機", "compact-state lost");
    simpleButton.append(simpleSync);
    item.append(simpleCheck.label, simpleButton);
    $("channels-simple").append(item);
    rows.set(c.id, {
      tr,
      check,
      format,
      sync,
      voice,
      data,
      button,
      simple: {
        item,
        check: simpleCheck.check,
        button: simpleButton,
        sync: simpleSync,
      },
    });
  }
  select(channels.find((c) => c.id === selected)?.id ?? channels[0]?.id ?? "");
  setControls();
  filterRows();
}
function renderChannelStates() {
  const stats = new Map(
    lastSnapshot?.channels.map((c) => [c.config.id, c]) ?? [],
  );
  for (const [id, r] of rows) {
    const c = stats.get(id);
    const included = enabled.has(id);
    const locked = included && running && !c?.error && c?.status.frame_locked;
    const label = !included
      ? "対象外"
      : c?.error
        ? "エラー"
        : !running
          ? lastSnapshot
            ? "停止"
            : "待機"
          : lastSnapshot?.active_calls?.includes(id)
            ? "通話中"
            : locked
              ? "同期"
              : lastSnapshot?.state === "receiving"
                ? "探索"
                : "準備";
    const tone =
      c?.error && included ? "channel-error" : locked ? "locked" : "lost";
    r.sync.textContent = r.simple.sync.textContent = label;
    r.sync.className = tone;
    r.simple.sync.className = `compact-state ${tone}`;
    r.simple.item.classList.toggle("excluded", !included);
    r.voice.textContent = c ? c.status.voice_frames.toLocaleString() : "—";
    r.data.textContent = c ? c.status.data.valid_frames.toLocaleString() : "—";
  }
}
function setChannelView(simple) {
  $("channels-simple").hidden = !simple;
  $("channels-table").hidden = simple;
  $("view-simple").setAttribute("aria-pressed", String(simple));
  $("view-detailed").setAttribute("aria-pressed", String(!simple));
}
$("view-simple").addEventListener("click", () => setChannelView(true));
$("view-detailed").addEventListener("click", () => setChannelView(false));
function select(id, manual = false) {
  if (manual) {
    selectionRevision++;
    $("auto-select").checked = false;
    renderSelectionMode();
  }
  if (selected !== id) resetAudio();
  selected = id;
  syncNativeMonitor();
  messageNodes.clear();
  $("messages").replaceChildren();
  for (const [key, r] of rows) {
    r.tr.classList.toggle("selected", key === id);
    r.simple.item.classList.toggle("selected", key === id);
    for (const button of [r.button, r.simple.button])
      button.setAttribute("aria-pressed", String(key === id));
  }
  const c = channels.find((c) => c.id === id);
  const note = presetCatalog.find((p) => p.id === activePresetId)
    ?.channel_notes[id];
  $("selected-format").value = c?.format ?? "slotted";
  messagesPaused = false;
  updatePause();
  $("selected-frequency").textContent = c
    ? (c.frequency_hz / 1e6).toFixed(5)
    : "—";
  const lines = c ? channelLines(c.id) : "";
  $("selected-label").textContent = c
    ? c.label +
      (lines ? ` · 使用例：${lines}` : "") +
      (note ? ` · ${note}` : "")
    : "プリセットを読み込んでください";
  const summary = c
    ? `${(c.frequency_hz / 1e6).toFixed(5)} MHz · ${c.label}`
    : "プリセットを読み込んでください";
  $("monitor-summary").textContent = summary;
  $("message-channel").textContent = summary;
  detail(null);
  if (lastSnapshot) {
    const row = lastSnapshot.channels.find((c) => c.config.id === id);
    if (row) detail({ ...row.status, messages: [] });
  }
}
function constellation(points = []) {
  const canvas = $("constellation"),
    ctx = canvas.getContext("2d"),
    w = canvas.width,
    h = canvas.height;
  ctx.clearRect(0, 0, w, h);
  const palette = getComputedStyle(document.documentElement);
  ctx.strokeStyle = palette.getPropertyValue("--plot-grid");
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.moveTo(w / 2, 14);
  ctx.lineTo(w / 2, h - 14);
  ctx.moveTo(14, h / 2);
  ctx.lineTo(w - 14, h / 2);
  ctx.stroke();
  for (const radius of [40, 80]) {
    ctx.beginPath();
    ctx.arc(w / 2, h / 2, radius, 0, Math.PI * 2);
    ctx.stroke();
  }
  for (const x of [-1, 1])
    for (const y of [-1, 1]) {
      ctx.beginPath();
      ctx.arc(w / 2 + x * 56.6, h / 2 + y * 56.6, 5, 0, Math.PI * 2);
      ctx.stroke();
    }
  ctx.fillStyle = palette.getPropertyValue("--plot-signal");
  for (const [x, y] of points) {
    ctx.fillRect(w / 2 + x * 80 - 1, h / 2 - y * 80 - 1, 2.5, 2.5);
  }
  ctx.globalAlpha = 1;
}
function showMessages(messages) {
  if (messagesPaused) return;
  $("message-count").textContent = `${messages.length} 件`;
  if (!messages.length) {
    messageNodes.clear();
    const emptyKind = !channels.length
      ? "noPreset"
      : !enabled.has(selected)
        ? "excluded"
        : running
          ? "receiving"
          : lastSnapshot
            ? "finished"
            : "ready";
    if (
      $("messages").dataset.emptyKind !== emptyKind ||
      !$("messages").querySelector(".empty")
    ) {
      $("messages").dataset.emptyKind = emptyKind;
      const empty = el("div", undefined, "empty");
      empty.append(
        el(
          "h3",
          {
            noPreset: "プリセットを読み込んでください",
            excluded: "この波は受信対象外です",
            receiving: "データを待っています",
            finished: "この波の受信データはありません",
            ready: "受信の準備ができています",
          }[emptyKind],
        ),
        el(
          "p",
          {
            noPreset:
              "チャンネル一覧のプリセットファイル設定で、再読み込みまたはファイルの選択ができます。",
            excluded:
              "受信を停止し、一覧のチェックを入れると、この波も復号できます。",
            receiving:
              "RS / CRC を通過した旅客情報・列車登録などが届くと表示します。",
            finished: "受信設定やデータ形式を確認し、受信を再開してください。",
            ready:
              "受信ソースを確認し、画面上部の「受信を開始」を選んでください。",
          }[emptyKind],
        ),
      );
      $("messages").replaceChildren(empty);
    }
    return;
  }
  $("messages").querySelector(".empty")?.remove();
  const retained = new Set();
  for (const item of messages) {
    const m = item.message,
      key = `${m.kind}:${m.identity}:${m.raw_hex}`;
    retained.add(key);
    let entry = messageNodes.get(key);
    if (!entry) {
      const root = el("details", undefined, "message"),
        summary = el("summary"),
        meta = el("small"),
        list = el("dl");
      for (const f of m.fields) {
        const dd = el("dd", f.value);
        if (f.english) dd.append(el("span", f.english, "en"));
        list.append(el("dt", fieldNames[f.field] ?? f.field), dd);
      }
      const raw = el("details");
      raw.append(el("summary", "生データを表示"), el("pre", m.raw_hex, "raw"));
      root.append(summary, meta, list, raw);
      entry = { root, summary, meta };
      messageNodes.set(key, entry);
      $("messages").append(root);
    }
    const description = m.fields.find((f) =>
      ["Description", "NoticeText", "TrainNumber"].includes(f.field),
    )?.value;
    entry.summary.textContent = `${kinds[m.kind] ?? m.kind} · ${m.identity}${description ? " · " + description.slice(0, 55) + (description.length > 55 ? "…" : "") : ""}`;
    entry.meta.textContent = `${m.timestamp ? "電文日時 " + m.timestamp.replace("T", " ") : "受信位置 " + (m.last_bit / 9600).toFixed(2) + " 秒"} · ${item.receptions} 回受信`;
  }
  for (const [key, entry] of messageNodes) {
    if (!retained.has(key)) {
      entry.root.remove();
      messageNodes.delete(key);
    }
  }
}
function detail(s) {
  $("sync").textContent = !enabled.has(selected)
    ? "対象外"
    : s?.frame_locked && running
      ? "同期中"
      : running
        ? "探索中"
        : "待機";
  $("sync").classList.toggle("locked", !!s?.frame_locked && running);
  $("level").textContent = s?.iq_samples
    ? `${s.input_dbfs.toFixed(1)} dBFS`
    : "—";
  $("evm").textContent = s?.constellation.length
    ? `${s.evm_percent.toFixed(1)} %`
    : "—";
  $("offset").textContent = s?.constellation.length
    ? `${s.frequency_error_hz.toFixed(0)} Hz`
    : "—";
  $("frames").textContent = s ? s.frames.toLocaleString() : "—";
  $("voice-count").textContent = s ? s.voice_frames.toLocaleString() : "—";
  const row = lastSnapshot?.channels.find((c) => c.config.id === selected);
  $("drops").textContent = s
    ? `${row?.dropped_blocks ?? 0} / ${s.discontinuities}`
    : "—";
  $("audio-drops").textContent = s
    ? (row?.audio_drops ?? 0).toLocaleString()
    : "—";
  $("data-status").textContent = s
    ? `有効 ${s.data.valid_frames} · 訂正 ${s.data.corrected_frames} · 電文拒否 ${s.data.rejected_messages}`
    : "RS / CRC 検証済みの情報を表示";
  constellation(s?.constellation);
  showMessages(s?.messages ?? []);
}
async function scan() {
  try {
    busy = true;
    $("scan").textContent = "検索中…";
    setControls();
    const current = $("args").value;
    const devices = await invoke("devices");
    $("device").replaceChildren();
    if (!devices.length) {
      $("device").append(el("option", "デバイスなし — 接続・ドライバーを確認"));
      $("device").options[0].value = "";
    }
    for (const d of devices) {
      const option = el("option", d.label);
      option.value = d.args;
      $("device").append(option);
    }
    if (current && !devices.some((d) => d.args === current)) {
      const option = el("option", "保存済みのデバイス（未検出）");
      option.value = current;
      $("device").append(option);
    }
    if (current) $("device").value = current;
    $("args").value = $("device").value;
    void persistPreferences();
  } catch (e) {
    fail(e);
  } finally {
    busy = false;
    $("scan").textContent = "検索";
    setControls();
  }
}
async function loadPresets(applyTuning = false) {
  const id = $("region").value;
  const wasBusy = busy;
  busy = true;
  setControls();
  try {
    const preset = presetCatalog.find((p) => p.id === id);
    if (!preset) throw new Error("受信プリセットが見つかりません。");
    const next = structuredClone(preset.channels);
    sessionVisible = false;
    lastSnapshot = null;
    uiGeneration++;
    resetAudio();
    channels = next;
    activePresetId = id;
    clearError();
    for (const metric of [
      "total-locked",
      "total-data",
      "total-voice",
      "total-drops",
    ])
      $(metric).textContent = "—";
    enabled = new Set(channels.map((c) => c.id));
    if (applyTuning && $("input-kind").value !== "wave") {
      $("center").value = (preset.center_hz / 1e6).toFixed(6);
      $("rate").value = preset.sample_rate;
    }
    $("band-low").textContent = (
      Math.min(...channels.map((c) => c.frequency_hz)) / 1e6
    ).toFixed(5);
    $("band-high").textContent = (
      Math.max(...channels.map((c) => c.frequency_hz)) / 1e6
    ).toFixed(5);
    $("band-note").textContent =
      `${channels.length} 波 · 音声とデータを並列復号`;
    $("all-label").textContent = `全 ${channels.length} 波を受信`;
    $("preset-note").textContent =
      `${preset.note} 変則割当は選択波の詳細に表示。件数は受信開始からの累計。`;
    $("capture-info").textContent =
      `中心 ${$("center").value} MHz · 設定は停止中に変更できます`;
    $("progress").textContent = "";
    $("state").textContent = "停止中";
    $("state").classList.remove("active");
    buildRows();
    updateSource();
  } catch (error) {
    $("region").value = activePresetId;
    throw error;
  } finally {
    busy = wasBusy;
    setControls();
  }
}
function renderPresetSource() {
  $("preset-source").textContent = presetFile ? "外部 JSON" : "組み込み";
  $("preset-path").textContent = presetFile ?? "";
  $("preset-path").hidden = !presetFile;
}
function showPresetError(error) {
  $("preset-status").textContent = "";
  $("preset-error").textContent =
    `プリセットを読み込めませんでした。${error.message ?? error} ${channels.length ? "現在のプリセットを引き続き使用します。" : "読み込むまで受信を開始できません。"} ファイルを確認して選び直してください。${presetFile ? "再読み込み、組み込みへの切替もできます。" : ""}`;
  $("preset-error").hidden = false;
  $("channels-panel").open = true;
  $("preset-settings").open = true;
}
function installCatalog(catalog, preferred) {
  const retained = catalog.some((p) => p.id === preferred);
  presetCatalog = catalog;
  $("region").replaceChildren(
    ...catalog.map((p) => {
      const option = el("option", p.label);
      option.value = p.id;
      return option;
    }),
  );
  $("region").value = retained ? preferred : catalog[0].id;
  $("preset-status").textContent = retained
    ? ""
    : `先頭のプリセット「${catalog[0].label}」を選択しました。`;
  return retained;
}
async function changePresetFile(action) {
  if (!native || running || busy) return;
  busy = true;
  setControls();
  $("preset-settings").setAttribute("aria-busy", "true");
  try {
    let path = action === "builtin" ? null : presetFile;
    if (action === "browse") {
      path = await invoke("plugin:dialog|open", {
        options: {
          title: "受信プリセットの JSON ファイルを選択",
          multiple: false,
          directory: false,
          filters: [{ name: "受信プリセット JSON", extensions: ["json"] }],
        },
      });
      if (typeof path !== "string") return;
    }
    // Fetch and validate first. A failed selection or reload keeps the current
    // catalog, tuning, channel choices, and persisted path intact.
    $("preset-status").textContent = "プリセットファイルを読み込み中…";
    const catalog = path
      ? await invoke("preset_catalog", { path })
      : bundledCatalog;
    const recovering = channels.length === 0;
    const retained = installCatalog(catalog, $("region").value);
    presetFile = path;
    await loadPresets(!recovering || !retained);
    renderPresetSource();
    $("preset-error").hidden = true;
    if (retained)
      $("preset-status").textContent = path
        ? "外部プリセットを読み込みました。"
        : "組み込みプリセットに戻しました。";
    await persistPreferences();
  } catch (error) {
    showPresetError(error);
  } finally {
    busy = false;
    $("preset-settings").removeAttribute("aria-busy");
    setControls();
  }
}
for (const action of ["browse", "reload", "builtin"])
  $(`preset-${action}`).addEventListener("click", () =>
    changePresetFile(action),
  );
$("device").addEventListener("change", () => {
  $("args").value = $("device").value;
});
$("scan").addEventListener("click", scan);
$("region").addEventListener("change", () => loadPresets(true).catch(fail));
$("all").addEventListener("change", () => {
  enabled = new Set($("all").checked ? channels.map((c) => c.id) : []);
  for (const [id, r] of rows) r.check.checked = enabled.has(id);
  setControls();
});
function updateSource() {
  const wave = $("input-kind").value === "wave";
  const tcp = $("input-kind").value === "rtl_tcp";
  $("driver-group").hidden = !(tcp && platform.android);
  $("device-group").hidden = wave || tcp;
  $("tcp-group").hidden = !tcp;
  $("ppm-group").hidden = !tcp;
  $("soapy-options").hidden = wave || tcp;
  $("file-group").hidden = !wave;
  $("rate").disabled = wave;
  $("gain").disabled = wave;
  $("rate").title = wave ? "WAV ヘッダーから取得します" : "";
  $("gain").placeholder = tcp ? "自動" : "機器の既定値";
  const host = $("host").value.trim();
  const port = $("port").value.trim();
  $("source-summary").textContent = tcp
    ? host && port
      ? `RTL-TCP · ${host.includes(":") ? `[${host}]` : host}:${port}`
      : "RTL-TCP · 接続先未設定"
    : wave
      ? "IQ WAV ファイル"
      : "SoapySDR デバイス";
  $("source-hint").textContent = wave
    ? "I は左、Q は右のステレオ WAV。サンプルレートはファイルから取得します。"
    : tcp && platform.android
      ? `ドングルを挿して「RTL-SDR ドライバーを起動」を押し、ドライバーが起動したら「受信を開始」を押してください。${channels.length} 波を同時受信します。`
      : tcp
      ? `利用するサーバーの IP アドレスとポートを入力してください。接続設定は自動保存されます。${channels.length} 波を同時受信します。RTL-TCP は設定値の読み戻しとサーバー側の欠落通知に対応していません。`
      : "SoapySDR 対応デバイスを検索するか、詳細設定にデバイス引数を入力してください。接続設定は自動保存されます。";
}
$("input-kind").addEventListener("change", () => {
  clearError();
  updateSource();
});
$("host").addEventListener("input", updateSource);
function fillDriverEndpoint() {
  let changed = false;
  for (const id of ["host", "port"])
    if (!$(id).value.trim()) {
      $(id).value = androidDriver[id];
      changed = true;
    }
  return changed;
}
$("driver-start").addEventListener("click", async () => {
  clearError();
  if (fillDriverEndpoint()) void persistPreferences();
  updateSource();
  try {
    await invoke("start_rtl_driver", { port: Number($("port").value) });
    $("driver-hint").textContent =
      "ドライバーの起動を確認したら「受信を開始」を押してください。";
  } catch (e) {
    fail(e);
  }
});
// Android: drop the SoapySDR source and the desktop file/folder pickers, whose
// results (content:// URIs) cannot be opened as paths. Paths can still be typed.
function applyAndroidLayout() {
  $("input-kind").querySelector('option[value="sdr"]')?.remove();
  for (const id of ["browse", "record-browse", "preset-browse"])
    $(id).hidden = true;
}
function adoptAndroidDefaults() {
  let changed = false;
  if ($("input-kind").value !== "rtl_tcp" && $("input-kind").value !== "wave") {
    $("input-kind").value = "rtl_tcp";
    changed = true;
  }
  if ($("input-kind").value === "rtl_tcp" && fillDriverEndpoint())
    changed = true;
  if (changed) void persistPreferences();
}
$("port").addEventListener("input", updateSource);
$("browse").addEventListener("click", async () => {
  try {
    const path = await invoke("plugin:dialog|open", {
      options: {
        multiple: false,
        directory: false,
        filters: [{ name: "IQ WAV", extensions: ["wav"] }],
      },
    });
    if (typeof path === "string") {
      $("path").value = path;
      void persistPreferences();
    }
  } catch (e) {
    fail(e);
  }
});
$("start").addEventListener("click", async () => {
  let invalidField;
  try {
    uiGeneration++;
    clearError();
    const values = Object.fromEntries(
      [
        "center",
        "rate",
        "gain",
        "host",
        "port",
        "ppm",
        "path",
        "args",
        "antenna",
        "rx-channel",
      ].map((id) => [id, $(id).value]),
    );
    values.kind = $("input-kind").value;
    const activeChannels = channels.filter((c) => enabled.has(c.id));
    const input = makeInput(values, activeChannels);
    busy = true;
    setControls();
    resetAudio();
    await prepareAudio().catch((e) => {
      $("listen").checked = false;
      fail(
        `音声出力を開始できません (${e})。復号は継続します。「音声を聴く」で再試行できます。`,
      );
    });
    await flushPreferences();
    await invoke("start", {
      config: { input, channels: activeChannels },
    });
    running = true;
    syncBackgroundService();
    syncNativeMonitor();
    sessionReplay = input.kind === "wave";
    recordingStatus = emptyRecording();
    recordingResumePending = recordingEnabled;
    clearRecordingError();
    sessionVisible = true;
    lastSnapshot = null;
    messageNodes.clear();
    messagesPaused = false;
    updatePause();
    $("messages").replaceChildren();
    $("state").textContent = "接続中";
    $("source-panel").open = false;
  } catch (e) {
    fail(e);
    if (e.field) {
      invalidField = $(e.field);
      $("source-panel").open = true;
      const panel = invalidField.closest(".panel-disclosure");
      if (panel) panel.open = true;
      if (e.field === "args") $("soapy-options").open = true;
      invalidField.setAttribute("aria-invalid", "true");
      invalidField.setAttribute("aria-describedby", "field-error");
      $("field-error").textContent = e.message;
      $("field-error").hidden = false;
    }
  } finally {
    busy = false;
    setControls();
    if (invalidField) invalidField.focus();
    else if (running) $("stop").focus();
  }
});
$("stop").addEventListener("click", async () => {
  try {
    uiGeneration++;
    busy = true;
    recordingResumePending = false;
    setControls();
    resetAudio();
    if (recordingStatus.state === "recording") {
      recordingStatus = { ...recordingStatus, state: "stopping" };
      renderRecording();
    }
    recordingStatus = await invoke("stop");
    running = false;
    syncBackgroundService();
    syncNativeMonitor();
    $("state").textContent = "停止中";
    resetAudio();
  } catch (e) {
    fail(e);
  } finally {
    busy = false;
    setControls();
    if (!running) $("start").focus();
  }
});
$("listen").addEventListener("change", () => {
  audioNotice = "";
  syncNativeMonitor();
  resetAudio();
  prepareAudio().catch(fail);
  setControls();
});
function filterRows() {
  const query = $("channel-search").value.trim().toLocaleLowerCase();
  let count = 0;
  for (const c of channels) {
    const shown = `${(c.frequency_hz / 1e6).toFixed(5)} ${c.label} ${channelLines(c.id)}`
      .toLocaleLowerCase()
      .includes(query);
    const row = rows.get(c.id);
    row.tr.hidden = row.simple.item.hidden = !shown;
    if (shown) count++;
  }
  $("no-channels").hidden = count !== 0 || !channels.length;
  $("search-status").textContent =
    `${count} 波を表示。受信対象は ${enabled.size} 波です。`;
}
$("channel-search").addEventListener("input", filterRows);
$("clear-search").addEventListener("click", () => {
  $("channel-search").value = "";
  filterRows();
  $("channel-search").focus();
});
$("selected-format").addEventListener("change", () => {
  const channel = channels.find((c) => c.id === selected);
  channel.format = $("selected-format").value;
  rows.get(selected).format.textContent =
    channel.format === "common" ? "一斉" : "通常";
  setControls();
});
function updatePause() {
  $("pause-messages").textContent = messagesPaused
    ? "更新を再開"
    : "更新を一時停止";
  $("pause-messages").setAttribute("aria-pressed", String(messagesPaused));
  $("message-scope").textContent = messagesPaused
    ? "表示を一時停止中 · 受信と復号は継続"
    : "選択波の最新 128 件まで";
}
$("pause-messages").addEventListener("click", () => {
  messagesPaused = !messagesPaused;
  updatePause();
  if (!messagesPaused)
    showMessages(
      lastSnapshot?.channels.find((c) => c.config.id === selected)?.status
        .messages ?? [],
    );
});
$("volume").addEventListener("input", () => {
  $("volume-value").value = `${$("volume").value}%`;
  if (gain) gain.gain.value = Number($("volume").value) / 100;
  syncNativeMonitor();
});
async function poll() {
  const started = performance.now();
  try {
    if (native && !busy && sessionVisible) {
      const generation = uiGeneration;
      const requestedChannel = selected;
      const revision = selectionRevision;
      const automatic = $("auto-select").checked;
      const result = await invoke("snapshot", {
        selected: requestedChannel,
        automatic,
        ...(nativeAudio ? { audio: false } : {}),
      });
      if (result && generation === uiGeneration) {
        const s = result.status;
        lastSnapshot = s;
        if (s.recording) recordingStatus = s.recording;
        running = s.running;
        syncBackgroundService();
        handleOutputLost(result.audio_output_lost_events);
        const selectionCurrent = revision === selectionRevision;
        if (
          selectionCurrent &&
          automatic &&
          s.selected_channel &&
          s.selected_channel !== selected &&
          channels.some((c) => c.id === s.selected_channel)
        ) {
          select(s.selected_channel);
        }
        // Feed the audio clock before rendering message history and channel
        // details. A stale selection response must not clear newer playback.
        if (selectionCurrent) {
          if (s.state === "receiving" || s.state === "completed")
            play(result.audio);
          else resetAudio();
          syncNativeMonitor();
        }
        const stateLabel =
          {
            starting: "準備中",
            receiving: "受信中",
            stopped: "停止中",
            completed: "再生完了",
            failed: "受信エラー",
          }[s.state] ?? s.state;
        if ($("state").textContent !== stateLabel)
          $("state").textContent = stateLabel;
        $("state").classList.toggle("active", s.running);
        if (s.error) fail(s.error);
        $("capture-info").textContent = s.sample_rate
          ? `${$("input-kind").value === "rtl_tcp" ? "要求中心" : "実効中心"} ${(s.center_hz / 1e6).toFixed(6)} MHz · ${s.sample_rate.toLocaleString()} S/s · ${$("input-kind").value === "rtl_tcp" ? "サーバー側欠落：通知なし" : "ソース欠落 " + s.source_overflows}`
          : s.running
            ? "接続を準備しています"
            : "未接続 · 受信設定と接続先を確認してください";
        $("total-locked").textContent = s.running
          ? s.channels.filter((c) => c.status.frame_locked).length
          : "—";
        $("total-data").textContent = s.channels
          .reduce((n, c) => n + c.status.data.valid_frames, 0)
          .toLocaleString();
        $("total-voice").textContent = s.channels
          .reduce((n, c) => n + c.status.voice_frames, 0)
          .toLocaleString();
        $("total-drops").textContent = s.channels
          .reduce((n, c) => n + c.dropped_blocks, 0)
          .toLocaleString();
        $("progress").textContent =
          $("input-kind").value === "wave"
            ? `再生 ${(s.progress * 100).toFixed(0)} %`
            : "";
        const c = s.channels.find((c) => c.config.id === selected);
        if (selectionCurrent) detail(c?.status ?? null);
        if (c?.error) fail(c.error);
        setControls();
        if (recordingResumePending) {
          if (!s.running) recordingResumePending = false;
          else if (s.state === "receiving") {
            recordingResumePending = false;
            if (recordingEnabled && recordingStatus.state === "idle")
              await startRecording(true);
          }
        }
      }
    }
  } catch (e) {
    fail(e);
  } finally {
    // IPC and rendering time are part of the polling period, not an extra
    // delay added after it. Leave breathing room when a poll itself is slow.
    setTimeout(poll, Math.max(20, 180 - (performance.now() - started)));
  }
}
busy = true;
setControls();
for (const id of [
  ...Object.values(preferenceFields),
  "device",
  "record-directory",
  "output-audio",
  "output-raw",
  "output-data",
]) {
  for (const event of ["input", "change"])
    $(id).addEventListener(event, () => {
      void persistPreferences();
    });
}
try {
  if (native) {
    try {
      platform = { ...platform, ...((await invoke("platform")) ?? {}) };
    } catch {
      // Older backends have no platform command; they are desktop builds.
    }
    if (platform.android) applyAndroidLayout();
    nativeAudio = platform.android;
  }
  bundledCatalog = native
    ? await invoke("preset_catalog")
    : buildCatalog(
        await fetch("radio-presets.json").then((r) => {
          if (!r.ok) throw new Error("受信設定を読み込めませんでした。");
          return r.json();
        }),
      );
  await restorePreferences(true);
  if (platform.android) adoptAndroidDefaults();
  busy = false;
  setControls();
  updateSource();
  constellation();
  if (native) {
    if (window.__TAURI__.window) {
      const appWindow = window.__TAURI__.window.getCurrentWindow();
      let closing = false;
      await appWindow.onCloseRequested(async (event) => {
        if (closing) {
          event.preventDefault();
          return;
        }
        closing = true;
        // Tauri awaits this handler before its default window destruction.
        await flushPreferences();
      });
    }
    poll();
  } else {
    $("runtime-note").textContent =
      "ブラウザプレビュー · 受信にはデスクトップアプリを起動してください";
    $("runtime-note").hidden = false;
    $("scan").disabled = true;
    $("browse").disabled = true;
  }
} catch (error) {
  busy = true;
  setControls();
  fail(error);
}
