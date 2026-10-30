import { makeInput } from "./config.mjs";
import { buildCatalog } from "./presets.mjs";
import { AudioPlayer, Leveler } from "./audio.mjs";
import { OperationMonitorView } from "./operation-monitor.mjs";
import { RoutePicker } from "./route-picker.mjs";
import { friendlyError } from "./errors.mjs";
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
  serviceReceiving = false;
function syncNativeMonitor() {
  if (!nativeAudio) return;
  const settings = {
    enabled: running && $("listen").checked,
    selected: selected || null,
    automatic: $("auto-select").checked,
    volume: Number($("volume").value) / 100,
    level: $("auto-level").checked,
  };
  const next = JSON.stringify(settings);
  if (next === nativeMonitorState) return;
  nativeMonitorState = next;
  invoke("set_native_monitor", { settings }).catch((e) => {
    nativeMonitorState = "";
    fail(e);
  });
}
// Three rising tones through the chosen output, so the playback path can be
// checked without any radio signal (same as native_audio::test_tone).
function testTone() {
  const rate = 8000,
    tone = rate * 0.3,
    gap = rate * 0.1,
    fade = rate * 0.015;
  const pcm = [];
  for (const hz of [523.25, 659.25, 783.99]) {
    for (let i = 0; i < tone; i++) {
      const edge = Math.min(Math.min(i, tone - 1 - i) / fade, 1);
      pcm.push(Math.sin((2 * Math.PI * hz * i) / rate) * edge * 0.5);
    }
    for (let i = 0; i < gap; i++) pcm.push(0);
  }
  return pcm;
}
async function playTestSound() {
  if (nativeAudio) {
    await invoke("play_test_sound");
    return;
  }
  if (!context) {
    context = new AudioContext();
    gain = context.createGain();
    gain.connect(context.destination);
    audioPlayer = new AudioPlayer(context, gain);
  }
  await context.resume();
  const pcm = testTone();
  const buffer = context.createBuffer(1, pcm.length, 8000);
  buffer.getChannelData(0).set(pcm);
  const node = context.createBufferSource();
  node.buffer = buffer;
  node.connect(context.destination);
  node.start();
}
function addTestSound() {
  const row = el("div", undefined, "test-sound");
  const button = el("button", "テスト音を鳴らす");
  button.type = "button";
  button.id = "test-sound";
  const note = el("p", "", "field-hint");
  note.id = "test-sound-note";
  note.hidden = true;
  button.addEventListener("click", () => {
    playTestSound().catch((e) => {
      note.textContent = `テスト音を鳴らせません：${friendlyError(e)}`;
      note.hidden = false;
    });
  });
  row.append(button, note);
  document.querySelector(".audio-controls").append(row);
}
// In-app recordings browser (Android): the app-specific folder cannot be
// opened from the phone's file manager, so list and play recordings here.
let recordingUrl = null;
const jstTime = new Intl.DateTimeFormat("ja-JP", {
  timeZone: "Asia/Tokyo",
  month: "numeric",
  day: "numeric",
  hour: "2-digit",
  minute: "2-digit",
  second: "2-digit",
  hourCycle: "h23",
});
function recordingTime(name) {
  const m = /^recording-(\d{4})(\d{2})(\d{2})T(\d{2})(\d{2})(\d{2})/.exec(name);
  if (!m) return name;
  return jstTime.format(new Date(Date.UTC(+m[1], +m[2] - 1, +m[3], +m[4], +m[5], +m[6])));
}
// "352537500Hz-000001.wav" or, with channel names, "352537500Hz-D5_音声第1波-000001.wav".
function parseRecordingName(name) {
  const m = /^(\d+)Hz-(?:(.+)-)?0*(\d+)\.wav$/i.exec(name);
  if (!m) return null;
  const hz = Number(m[1]);
  const channel = channels.find((c) => c.frequency_hz === hz);
  return {
    hz,
    number: Number(m[3]),
    label: m[2]?.replaceAll("_", " ") ?? channel?.label ?? "",
    lines: channel ? shortLines(channelLineList(channel.id)) : "",
  };
}
// [A, B, C, D] → "A・B ほか" (one frequency serves many lines nationwide).
function shortLines(list, count = 2) {
  return list.length > count
    ? `${list.slice(0, count).join("・")} ほか`
    : list.join("・");
}
function recordingLabel(file) {
  const seconds = Math.max(0, (file.bytes - 44) / 16000);
  const length = seconds >= 1 ? `${Math.round(seconds)} 秒` : "1 秒未満";
  const p = parseRecordingName(file.name);
  return p
    ? [`${(p.hz / 1e6).toFixed(5)} MHz`, p.label, p.lines, `#${p.number}`, length]
        .filter(Boolean)
        .join(" · ")
    : `${file.name} · ${length}`;
}
// Where a recording plays (the recordings browser's player).
const browserPlayer = { player: "recordings-player", now: "recordings-now", status: "recordings-status" };
async function playRecording(session, file, target = browserPlayer) {
  try {
    $(target.status).textContent = "読み込み中…";
    const data = await invoke("read_recording", {
      path: file.path,
      directory: $("record-directory").value.trim() || null,
    });
    // Raw IPC responses arrive as an ArrayBuffer; older IPC paths send an array.
    const bytes = Array.isArray(data) ? new Uint8Array(data) : data;
    const blob = new Blob([bytes], { type: "audio/wav" });
    if (recordingUrl) URL.revokeObjectURL(recordingUrl);
    recordingUrl = URL.createObjectURL(blob);
    const player = $(target.player);
    player.src = recordingUrl;
    player.hidden = false;
    $(target.now).textContent = `再生中：${fileTime(session, file)} · ${recordingLabel(file)}`;
    $(target.now).hidden = false;
    $(target.status).textContent = session.incomplete ? "保存中のため途中までの場合があります" : "";
    await Promise.resolve(player.play?.()).catch(() => {});
  } catch (e) {
    $(target.status).textContent = friendlyError(e);
  }
}
// When the call ended (file time), or the session start when unknown.
function fileTime(session, file) {
  return file.modified
    ? jstTime.format(new Date(file.modified * 1000))
    : recordingTime(session.name);
}
const sortKey = "jr-clrr.recordings-sort";
try {
  const saved = localStorage.getItem(sortKey);
  if ([...$("recordings-sort").options].some((o) => o.value === saved))
    $("recordings-sort").value = saved;
} catch {
  // Default order.
}
let recordingSessions = [];
// The list grows by this many entries (sessions in "保存した回ごと") per
// "もっと見る".
const RECORDINGS_PAGE = 30,
  SESSIONS_PAGE = 10;
let recordingsShown = RECORDINGS_PAGE,
  sessionsShown = SESSIONS_PAGE;
function moreButton(left, onMore) {
  const button = el("button", `もっと見る（残り ${left.toLocaleString()} 件）`, "recordings-more");
  button.type = "button";
  button.id = "recordings-more";
  button.addEventListener("click", () => {
    onMore();
    renderRecordings();
  });
  return button;
}
function recordingButton(session, file, withTime, target = browserPlayer) {
  const text = `▶ ${withTime ? `${fileTime(session, file)} · ` : ""}${recordingLabel(file)}${session.incomplete ? " · 保存中" : ""}`;
  const button = el("button", text, "recording-play");
  button.type = "button";
  button.addEventListener("click", () => playRecording(session, file, target));
  return button;
}
function renderRecordings() {
  const list = $("recordings-list");
  list.replaceChildren();
  const order = $("recordings-sort").value;
  if (order === "session") {
    recordingSessions.slice(0, sessionsShown).forEach((session, i) => {
      const group = el("details", undefined, "recording-session");
      group.open = i === 0;
      group.append(
        el(
          "summary",
          `${recordingTime(session.name)} · ${session.files.length} 件${session.incomplete ? " · 保存中" : ""}`,
        ),
      );
      for (const file of session.files) group.append(recordingButton(session, file, false));
      list.append(group);
    });
    if (recordingSessions.length > sessionsShown)
      list.append(
        moreButton(recordingSessions.length - sessionsShown, () => (sessionsShown += SESSIONS_PAGE)),
      );
    return;
  }
  const all = recordingSessions.flatMap((session) =>
    session.files.map((file) => ({ session, file, info: parseRecordingName(file.name) })),
  );
  const newest = (a, b) => b.file.modified - a.file.modified || b.file.name.localeCompare(a.file.name);
  all.sort(
    order === "oldest"
      ? (a, b) => -newest(a, b)
      : order === "channel"
        ? (a, b) => (a.info?.hz ?? 0) - (b.info?.hz ?? 0) || newest(a, b)
        : newest,
  );
  let heading = null;
  for (const { session, file, info } of all.slice(0, recordingsShown)) {
    if (order === "channel") {
      const title = info
        ? [`${(info.hz / 1e6).toFixed(5)} MHz`, info.label, info.lines].filter(Boolean).join(" · ")
        : "その他";
      if (title !== heading) {
        heading = title;
        list.append(el("h3", title, "recording-heading"));
      }
    }
    list.append(recordingButton(session, file, true));
  }
  if (all.length > recordingsShown)
    list.append(moreButton(all.length - recordingsShown, () => (recordingsShown += RECORDINGS_PAGE)));
}
function resetRecordingsPaging() {
  recordingsShown = RECORDINGS_PAGE;
  sessionsShown = SESSIONS_PAGE;
}
async function refreshRecordings() {
  resetRecordingsPaging();
  $("recordings-status").textContent = "読み込み中…";
  try {
    recordingSessions =
      (await invoke("list_recordings", {
        directory: $("record-directory").value.trim() || null,
      })) ?? [];
    const total = recordingSessions.reduce((n, s) => n + s.files.length, 0);
    $("recordings-count").textContent = `${recordingSessions.length} 回の保存 · ${total} 件`;
    $("recordings-status").textContent = recordingSessions.length ? "" : "録音はまだありません";
    renderRecordings();
  } catch (e) {
    $("recordings-status").textContent = friendlyError(e);
  }
}
$("recordings-sort").addEventListener("change", () => {
  try {
    localStorage.setItem(sortKey, $("recordings-sort").value);
  } catch {
    // The order still applies now.
  }
  resetRecordingsPaging();
  renderRecordings();
});
function addRecordingsBrowser() {
  $("recordings-browser").hidden = false;
  $("recordings-browser").addEventListener("toggle", () => {
    if ($("recordings-browser").open) void refreshRecordings();
  });
  $("recordings-refresh").addEventListener("click", () => void refreshRecordings());
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
const routePicker = new RoutePicker(document, async (preset, selection) => {
  if (running || busy) throw new Error("受信を停止してから適用してください。");
  if (preset.id !== activePresetId) {
    $("region").value = preset.id;
    await loadPresets(true);
  }
  enabled = new Set(selection.channels.map((c) => c.id));
  saveSelection();
  // Reception targets do not change the listener's manual audio selection.
  const names = selection.routes.slice(0, 2).map((r) => `${r.line}（${r.section}）`).join("、");
  const more = selection.routes.length > 2 ? ` ほか${selection.routes.length - 2}区間` : "";
  $("route-feedback").textContent =
    `${names}${more} · ${enabled.size} 波を受信対象にしました。「受信を開始」で受信します。`;
  setControls();
  filterRows();
  await persistPreferences();
});
$("route-open").addEventListener("click", () => routePicker.open(activePresetId, enabled));
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
  const text = friendlyError(error);
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
  renderLive();
  $("route-open").disabled = busy || !presetCatalog.length;
  routePicker.setBlocked(running || busy);
  $("settings").disabled = running || busy;
  $("region").disabled = running || busy || !channels.length;
  $("preset-browse").disabled = !native || running || busy;
  $("preset-reload").disabled = !native || running || busy || !presetFile;
  $("preset-builtin").disabled = !native || running || busy || !presetFile;
  $("all").disabled = running || busy || !channels.length;
  $("start").disabled = !native || busy || (!running && !channels.length);
  const startLabel = running ? "■ 受信を停止" : "▶ 受信を開始";
  if ($("start").textContent !== startLabel) $("start").textContent = startLabel;
  $("start").classList.toggle("primary", !running);
  $("start").classList.toggle("stop", running);
  $("selected-format").disabled = running || busy || !channels.length;
  $("preferences-retry").disabled = busy || (preferencesLoadFailed && running);
  $("total-enabled").textContent = enabled.size;
  renderMute();
  $("audio-state").textContent = !$("listen").checked
    ? "消音中"
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
  renderQuick();
  operationMonitor.update(lastSnapshot, { channels, enabled, running, replay: sessionReplay });
}
// JR-CLRR builds: 簡易 (compact) shows what matters on one phone screen —
// the channel being heard, calls in progress, totals and saving; 詳細 (full)
// is the whole page. Kept per device; phones start compact.
const viewKey = "jr-clrr.view";
function setViewMode(compact, remember = true) {
  document.documentElement.classList.toggle("compact", compact);
  $("quick").hidden = !compact;
  $("mode-compact").setAttribute("aria-pressed", String(compact));
  $("mode-full").setAttribute("aria-pressed", String(!compact));
  if (remember)
    try {
      localStorage.setItem(viewKey, compact ? "compact" : "full");
    } catch {
      // The choice still applies now.
    }
  renderQuick();
}
function addViewMode() {
  $("view-mode").hidden = false;
  let saved = null;
  try {
    saved = localStorage.getItem(viewKey);
  } catch {
    // Default for the platform.
  }
  setViewMode(saved ? saved === "compact" : !!platform.android, false);
}
$("mode-compact").addEventListener("click", () => setViewMode(true));
$("mode-full").addEventListener("click", () => setViewMode(false));
function setText(id, text) {
  if ($(id).textContent !== text) $(id).textContent = text;
}
function renderQuick() {
  if ($("quick").hidden) return;
  const s = running ? lastSnapshot : null;
  const c = channels.find((x) => x.id === selected);
  const row = s?.channels.find((x) => x.config.id === selected);
  const calling = (s?.active_calls ?? []).includes(selected);
  setText("quick-frequency", c ? (c.frequency_hz / 1e6).toFixed(5) : "—");
  setText("quick-channel", c?.label ?? "—");
  setText("quick-lines", c ? shortLines(channelLineList(c.id), 3) : "");
  const locked = !!row?.status.frame_locked;
  setText(
    "quick-sync",
    !c
      ? ""
      : !enabled.has(selected)
        ? "受信対象外"
        : !running
          ? "待機"
          : calling
            ? "通話中"
            : locked
              ? "同期中"
              : "探索中",
  );
  $("quick-sync").classList.toggle("locked", running && locked);
  $("quick-sync").classList.toggle("calling", running && calling);
  const count = (f) => (s ? s.channels.reduce((n, x) => n + f(x), 0).toLocaleString() : "—");
  setText("quick-locked", `${s ? s.channels.filter((x) => x.status.frame_locked).length : "—"}/${enabled.size}`);
  setText("quick-voice", count((x) => x.status.voice_frames));
  setText("quick-drops", count((x) => x.dropped_blocks));
  const r = recordingStatus;
  setText(
    "quick-recording",
    r.state === "recording" || r.state === "stopping"
      ? `${r.files} 件`
      : r.state === "failed"
        ? "エラー"
        : recordingEnabled
          ? "オン"
          : "オフ",
  );
  $("quick-recording").classList.toggle("active", r.state === "recording");
  $("quick-auto").checked = $("auto-select").checked;
  $("quick-level").checked = $("auto-level").checked;
  if ($("quick-volume").value !== $("volume").value) $("quick-volume").value = $("volume").value;
  $("quick-routes").disabled = $("route-open").disabled;
}
// The compact card's controls drive the full page's ones.
$("quick-auto").addEventListener("change", () => {
  $("auto-select").checked = $("quick-auto").checked;
  $("auto-select").dispatchEvent(new Event("change"));
});
$("quick-level").addEventListener("change", () => {
  $("auto-level").checked = $("quick-level").checked;
  $("auto-level").dispatchEvent(new Event("change"));
});
$("quick-volume").addEventListener("input", () => {
  $("volume").value = $("quick-volume").value;
  $("volume").dispatchEvent(new Event("input"));
});
$("quick-routes").addEventListener("click", () => $("route-open").click());
// Receive targets, the monitored channel and automatic selection are kept per
// preset on this device, so a restart (or reinstall of settings) keeps them.
const selectionKey = () => `jr-clrr.selection.${activePresetId}`;
function saveSelection() {
  if (!channels.length) return;
  try {
    localStorage.setItem(
      selectionKey(),
      JSON.stringify({
        enabled: channels.filter((c) => enabled.has(c.id)).map((c) => c.id),
        selected,
        automatic: $("auto-select").checked,
      }),
    );
  } catch {
    // Not persisted; the selection still applies now.
  }
}
function restoreSelection() {
  let saved = null;
  try {
    saved = JSON.parse(localStorage.getItem(selectionKey()) ?? "null");
  } catch {
    return;
  }
  if (!saved || !Array.isArray(saved.enabled)) return;
  const ids = new Set(channels.map((c) => c.id));
  enabled = new Set(saved.enabled.filter((id) => ids.has(id)));
  if (ids.has(saved.selected)) selected = saved.selected;
  if (typeof saved.automatic === "boolean") {
    $("auto-select").checked = saved.automatic;
    renderSelectionMode();
  }
}
function renderSelectionMode() {
  const automatic = $("auto-select").checked;
  const hint = automatic ? "" : "手動で選んだ波に固定中";
  if ($("selection-hint").textContent !== hint)
    $("selection-hint").textContent = hint;
}
$("auto-select").addEventListener("change", () => {
  selectionRevision++;
  renderSelectionMode();
  saveSelection();
  syncNativeMonitor();
});
const clipRules = { min_clip_ms: 1000, join_resync_ms: 2000, channel_names: true };
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
  // Top bar toggle: on = saving now, or from the next reception.
  const on = recordingEnabled || active;
  const toggle = $("save-toggle");
  toggle.disabled = !native || busy || s.state === "stopping";
  toggle.setAttribute("aria-pressed", String(on));
  toggle.classList.toggle("saving", active);
  const toggleLabel = active ? "● 保存中" : on ? "● 保存オン" : "保存オフ";
  if (toggle.textContent !== toggleLabel) toggle.textContent = toggleLabel;
  // Sessions without any call (or selected JSONL row) leave no folder.
  const saved = s.files > 0 || (s.raw_frames ?? 0) > 0 || (s.data_messages ?? 0) > 0;
  const label =
    s.state === "completed" && s.discarded
        ? "通話なし"
        : ({
            idle: recordingEnabled ? "保存待機" : "未保存",
            recording: "保存中",
            stopping: "保存を終了中",
            completed: "保存完了",
            failed: "保存エラー",
          }[s.state] ?? s.state);
  if ($("recording-state").textContent !== label)
    $("recording-state").textContent = label;
  $("recording-state").classList.toggle("active", active);
  const summary =
    s.state === "idle"
      ? recordingEnabled
        ? "受信を始めると保存します"
        : ""
      : s.discarded
        ? "通話がなかったため保存しませんでした"
        : s.state === "recording" && !saved
          ? "通話を待っています"
          : [
              `WAV ${s.files} 件 · ${(s.pcm_samples / 8000).toFixed(1)} 秒`,
              s.raw_frames ? `生フレーム ${s.raw_frames} 件` : "",
              s.data_messages ? `データ電文 ${s.data_messages} 件` : "",
            ]
              .filter(Boolean)
              .join(" · ");
  $("recording-summary").textContent = summary;
  if (s.discarded) $("recording-output").hidden = true;
  else if (s.directory) {
    $("recording-output").textContent =
      `${active ? "保存フォルダー" : "保存したフォルダー"}：${s.directory}`;
    $("recording-output").hidden = false;
  }
  const error = recordingActionError || s.error;
  const message = error ? friendlyError(error) : "";
  if ($("recording-error").textContent !== message) {
    $("recording-error").textContent = message;
    if (error) $("recording-settings").open = true;
  }
  $("recording-error").hidden = !error;
}
function clearRecordingError() {
  recordingActionError = "";
  $("record-directory").removeAttribute("aria-invalid");
  $("record-directory").removeAttribute("aria-describedby");
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
    recordingStatus = await invoke("start_recording", {
      directory,
      // Weak signals resync mid-call and decode isolated noise frames: keep a
      // call in one file and drop WAVs shorter than one second.
      options: { ...options, ...clipRules },
    });
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
$("record-stop").addEventListener("click", () => stopSaving(true));
async function stopSaving(fromCard = false) {
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
    if (fromCard && !$("record-start").disabled) $("record-start").focus();
  }
}
// Top bar: saving on/off. Turning it on while receiving saves now; while
// stopped (or still connecting) it saves from the next reception, as the
// setting is kept.
$("save-toggle").addEventListener("click", async () => {
  if (recordingEnabled || ["recording", "stopping"].includes(recordingStatus.state))
    return stopSaving();
  if (running && lastSnapshot?.state === "receiving") return startRecording();
  clearRecordingError();
  if (!$("record-directory").value.trim()) {
    recordingActionError = "保存先フォルダーを選択してください。";
    fail(recordingActionError);
    renderRecording();
    return;
  }
  if (!Object.values(outputOptions()).some(Boolean)) {
    recordingActionError = "保存する内容を 1 つ以上選択してください。";
    fail(recordingActionError);
    renderRecording();
    return;
  }
  recordingEnabled = true;
  if (running) recordingResumePending = true;
  await flushPreferences();
  setControls();
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
  applyLeveling();
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
    $("route-feedback").textContent = "";
    saveSelection();
    setControls();
  });
  const label = el("label", undefined, "row-check");
  label.append(check);
  return { check, label };
}
// Lines whose listed sections use this frequency, from the preset's route
// table (docs/ROUTES.ja.md). The common wave serves every line.
function channelLines(id) {
  return channelLineList(id).join("・");
}
function channelLineList(id) {
  const preset = presetCatalog.find((p) => p.id === activePresetId);
  const channel = channels.find((c) => c.id === id);
  if (!preset || !channel) return [];
  if (channel.format === "common") return ["全線区共通"];
  const hz = channel.frequency_hz;
  const lines = new Set(
    (preset.routes ?? [])
      .filter((r) => r.data_hz === hz || r.voice_hz.includes(hz))
      .map((r) => r.line),
  );
  return [...lines];
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
    node.title = lines;
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
  if (manual) saveSelection();
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
      (lines ? ` · ${lines}` : "") +
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
  renderQuick();
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
    : "";
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
    $("route-feedback").textContent = "";
    clearError();
    for (const metric of [
      "total-locked",
      "total-data",
      "total-voice",
      "total-drops",
    ])
      $(metric).textContent = "—";
    enabled = new Set(channels.map((c) => c.id));
    restoreSelection();
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
    $("band-note").textContent = `割当 ${channels.length} 波`;
    $("all-label").textContent = `全 ${channels.length} 波を受信`;
    $("preset-note").textContent = preset.note;
    $("capture-info").textContent = `中心 ${$("center").value} MHz`;
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
  routePicker.setCatalog(catalog);
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
  $("route-feedback").textContent = "";
  enabled = new Set($("all").checked ? channels.map((c) => c.id) : []);
  for (const [id, r] of rows) r.check.checked = enabled.has(id);
  saveSelection();
  setControls();
});
function updateSource() {
  const wave = $("input-kind").value === "wave";
  const tcp = $("input-kind").value === "rtl_tcp";
  const usb = $("input-kind").value === "usb";
  $("driver-group").hidden = !(tcp && platform.android);
  $("device-group").hidden = wave || tcp || usb;
  $("tcp-group").hidden = !tcp;
  $("ppm-group").hidden = !(tcp || usb);
  $("soapy-options").hidden = wave || tcp || usb;
  $("file-group").hidden = !wave;
  $("rate").disabled = wave;
  $("gain").disabled = wave;
  $("rate").title = wave ? "WAV ヘッダーから取得します" : "";
  $("gain").placeholder = tcp || usb ? "自動" : "機器の既定値";
  const host = $("host").value.trim();
  const port = $("port").value.trim();
  $("source-summary").textContent = usb
    ? "USB 直結（内蔵ドライバー）"
    : tcp
    ? host && port
      ? `RTL-TCP · ${host.includes(":") ? `[${host}]` : host}:${port}`
      : "RTL-TCP · 接続先未設定"
    : wave
      ? "IQ WAV ファイル"
      : "SoapySDR デバイス";
}
// The last session stays in the receiver after it ends, and polling it would
// show its error again; forget it once the user moves on.
function forgetSession() {
  if (running) return;
  sessionVisible = false;
  lastSnapshot = null;
  uiGeneration++;
  clearError();
  $("state").textContent = "停止中";
  $("state").classList.remove("active");
}
$("input-kind").addEventListener("change", () => {
  forgetSession();
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
// RTL-TCP reconnects by itself when the driver app stops streaming. If the
// on-phone driver does not answer (it closed its service), start it again;
// Android only lets an app open another app while it is on screen.
const driverRelaunch = { at: 0, count: 0 };
function relaunchDriverIfNeeded(s) {
  if (!s.reconnecting) {
    driverRelaunch.count = 0;
    return;
  }
  if (
    !platform.android ||
    $("input-kind").value !== "rtl_tcp" ||
    !["127.0.0.1", "localhost"].includes($("host").value.trim()) ||
    s.reconnect_failures < 1 ||
    driverRelaunch.count >= 3 ||
    Date.now() - driverRelaunch.at < 15000
  )
    return;
  driverRelaunch.at = Date.now();
  driverRelaunch.count++;
  invoke("start_rtl_driver", { port: Number($("port").value) }).catch(() => {});
}
$("driver-start").addEventListener("click", async () => {
  clearError();
  if (fillDriverEndpoint()) void persistPreferences();
  updateSource();
  try {
    await invoke("start_rtl_driver", { port: Number($("port").value) });
    $("driver-hint").textContent = "起動したら「受信を開始」";
  } catch (e) {
    fail(e);
  }
});
// Android: drop the SoapySDR source and the desktop file/folder pickers, whose
// results (content:// URIs) cannot be opened as paths. Paths can still be typed.
function applyAndroidLayout() {
  document.documentElement.classList.add("android");
  $("input-kind").querySelector('option[value="sdr"]')?.remove();
  // Built-in driver: the app drives the dongle itself (no driver app).
  const usb = el("option", "USB 直結（内蔵ドライバー）");
  usb.value = "usb";
  $("input-kind").prepend(usb);
  for (const id of ["browse", "record-browse", "preset-browse"])
    $(id).hidden = true;
}
// JR-CLRR builds (Android and macOS): no search features, only the detailed
// channel list, a mute toggle in the top bar, and the build's own name.
function applySimpleLayout(name) {
  document.documentElement.classList.add("simple");
  // Targets are chosen by route ("路線から選ぶ") or per channel; no search box.
  $("channel-search").closest("label").hidden = true;
  $("filter-note").hidden = true;
  // Detailed list only.
  setChannelView(false);
  $("view-detailed").closest(".view-switch").hidden = true;
  // Mute toggle next to start/stop; the listen checkbox stays the source of truth.
  const mute = el("button", undefined, "mute-toggle");
  mute.id = "mute-toggle";
  mute.type = "button";
  mute.addEventListener("click", () => {
    $("listen").checked = !$("listen").checked;
    $("listen").dispatchEvent(new Event("change"));
    renderMute();
  });
  $("state").after(mute);
  $("listen").closest(".listening-control").hidden = true;
  renderMute();
  // Name.
  document.title = name;
  document.querySelector(".topbar h1").textContent = name;
  document.querySelector(".topbar .eyebrow").textContent =
    "JR 在来線デジタル列車無線受信";
  $("about-product").textContent = name;
  if (platform.build) {
    const version = document.querySelector(".about-metadata dd");
    version.textContent = `${version.textContent} · ビルド ${platform.build.slice(0, 7)}`;
  }
  renderAbout();
  document.querySelector("footer > span").firstChild.textContent = `${name} `;
}
function renderAbout() {
  const edition = platform.android ? "Android 版" : "Mac 版";
  $("about-description").textContent =
    `KIRISHIKI Yudai 氏の JR Cconventional-line Radio Receiver をもとにした ${edition}（音声の復号は v1.5）`;
  // The dongle drivers this build ships with or works with.
  const credits = platform.android
    ? [
        "内蔵ドライバー：rtlsdr-nusb（bastibl、Apache-2.0）を改変して同梱",
        "チューナー設定の一部：librtlsdr（Osmocom・RTL-SDR Blog、GPL-2.0）を参照",
        "RTL-TCP：Rtl-sdr driver（Martin Marinov）に接続",
      ]
    : [
        "SoapySDR（Pothosware、BSL-1.0）",
        "SoapyRTLSDR（Pothosware、MIT）",
        "librtlsdr（Osmocom、GPL-2.0）",
        "libusb（LGPL-2.1）",
      ];
  $("about-driver").replaceChildren(...credits.map((line) => el("span", line)));
  $("about-driver").hidden = $("about-driver-title").hidden = false;
}
function renderMute() {
  const mute = $("mute-toggle");
  if (!mute) return;
  const muted = !$("listen").checked;
  mute.setAttribute("aria-pressed", String(muted));
  mute.textContent = muted ? "ミュート中" : "音声オン";
  mute.title = muted ? "タップで音声を再開" : "タップでミュート";
}
// The Mac app bundles SoapySDR and the RTL-SDR module: use the dongle directly
// unless an RTL-TCP server or an IQ file was set up on purpose.
function adoptMacDefaults() {
  if ($("input-kind").value === "rtl_tcp" && !$("host").value.trim()) {
    $("input-kind").value = "sdr";
    void persistPreferences();
  }
}
// What the phone saw when the built-in driver asked for the dongle.
function usbProblem(answer) {
  if (answer === "none")
    return "ドングルが見つかりません。挿し直して、OTG がオンか確認してください。";
  if (typeof answer === "string" && answer.startsWith("other:"))
    return "ドングルが見つかりません。ハブとドングルの差し込みを確認してください。";
  if (answer === "busy")
    return "ドングルを開けません。Rtl-sdr driver を閉じるか、挿し直してください。";
  return null;
}
const usbDefaultKey = "jr-clrr.usb-default";
function adoptAndroidDefaults() {
  let changed = false;
  // Switch to the built-in driver once; afterwards respect the user's choice.
  let switched = true;
  try {
    switched = localStorage.getItem(usbDefaultKey) === "done";
    localStorage.setItem(usbDefaultKey, "done");
  } catch {
    switched = false;
  }
  if (!switched && $("input-kind").value !== "wave") {
    $("input-kind").value = "usb";
    changed = true;
  }
  if (!["usb", "rtl_tcp", "wave"].includes($("input-kind").value)) {
    $("input-kind").value = "usb";
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
// One button starts and stops reception.
$("start").addEventListener("click", () => (running ? stopReception() : startReception()));
async function startReception() {
  let invalidField;
  try {
    uiGeneration++;
    clearError();
    forgetSession();
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
    await prepareAudio().catch(() => {
      $("listen").checked = false;
      fail("音声を再生できません。音声オンにすると再試行します。");
    });
    await flushPreferences();
    if (input.kind === "usb" && platform.android) {
      // Built-in driver: Android hands the dongle over (asking first if needed).
      const problem = usbProblem(window.JrAndroid?.requestUsb?.());
      if (problem) throw new Error(problem);
    }
    await invoke("start", {
      config: {
        input,
        channels: activeChannels,
        // The most natural-sounding decoder, on every build.
        voice_decoder: "v15",
      },
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
    else if (running) $("start").focus();
  }
}
async function stopReception() {
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
    $("start").focus();
  }
}
$("listen").addEventListener("change", () => {
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
// Wideband input level (same for every channel of the capture): very low
// means hardly any signal reaches the dongle's output.
function inputLevel(s) {
  const level = s.channels?.find((c) => c.status.iq_samples)?.status.input_dbfs;
  return Number.isFinite(level) ? ` · 入力 ${level.toFixed(1)} dBFS` : "";
}
// Samples per second actually arriving; far from the setting means the
// dongle is not running at the requested rate (or input is being lost).
function measuredRate(s) {
  if (!s.input_rate || $("input-kind").value === "wave") return "";
  const off = Math.abs(s.input_rate / s.sample_rate - 1) > 0.05;
  return ` · 実測 ${Math.round(s.input_rate).toLocaleString()} S/s${off ? "（設定と違います）" : ""}`;
}
// Main page: calls in progress (with background recording and playback state)
// and the latest recordings, playable in place.
let liveKey = "";
function renderLive() {
  const s = running ? lastSnapshot : null;
  const calls = s?.active_calls ?? [];
  const muted = !$("listen").checked;
  const recording = recordingStatus.state === "recording";
  const key = JSON.stringify([running, calls, selected, muted, recording]);
  if (key === liveKey) return;
  liveKey = key;
  $("live-calls").replaceChildren(
    ...calls.map((id) => {
      const ch = channels.find((x) => x.id === id);
      const li = el("li");
      const button = el("button", undefined, "live-call");
      button.type = "button";
      button.setAttribute("aria-pressed", String(id === selected));
      const name = el("span", undefined, "call-name");
      name.append(el("bdi", ch ? (ch.frequency_hz / 1e6).toFixed(5) : id), ch?.label ?? "");
      const lines = ch ? shortLines(channelLineList(id), 2) : "";
      if (lines) name.title = lines;
      const tags = el("span", undefined, "call-tags");
      if (recording) tags.append(el("span", "REC", "tag-rec"));
      if (id === selected && !muted) tags.append(el("span", "再生中", "tag-play"));
      button.append(el("span", undefined, "lamp"), name, tags);
      button.setAttribute(
        "aria-label",
        `${ch ? (ch.frequency_hz / 1e6).toFixed(5) + " MHz " + ch.label : id}${recording ? "、録音中" : ""}${id === selected && !muted ? "、再生中" : "。タップで聴く"}`,
      );
      button.addEventListener("click", () => {
        select(id, true);
        renderLive();
      });
      li.append(button);
      return li;
    }),
  );
  $("live-calls-empty").hidden = calls.length > 0;
  $("live-calls-empty").textContent = running ? "通話はありません" : "受信していません";
}
// Volume leveling (live playback only; recordings keep the original level).
// The checkbox once shared its id with the "広帯域入力" figure, so the page
// read the figure's (absent) checked state and never leveled.
const levelKey = "jr-clrr.level";
try {
  $("auto-level").checked = localStorage.getItem(levelKey) === "on";
} catch {
  // Not persisted; off by default.
}
function applyLeveling() {
  if (audioPlayer)
    audioPlayer.leveler = $("auto-level").checked ? (audioPlayer.leveler ?? new Leveler()) : null;
}
$("auto-level").addEventListener("change", () => {
  try {
    localStorage.setItem(levelKey, $("auto-level").checked ? "on" : "off");
  } catch {
    // The choice still applies now.
  }
  applyLeveling();
  syncNativeMonitor();
  setControls();
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
        const stateLabel = s.reconnecting
          ? "再接続中"
          : ({
              starting: "準備中",
              receiving: "受信中",
              stopped: "停止中",
              completed: "再生完了",
              failed: "受信エラー",
            }[s.state] ?? s.state);
        relaunchDriverIfNeeded(s);
        if ($("state").textContent !== stateLabel)
          $("state").textContent = stateLabel;
        $("state").classList.toggle("active", s.running);
        if (s.error) fail(s.error);
        $("capture-info").textContent = s.sample_rate
          ? `${$("input-kind").value === "rtl_tcp" ? "要求中心" : "実効中心"} ${(s.center_hz / 1e6).toFixed(6)} MHz · ${s.sample_rate.toLocaleString()} S/s · ${$("input-kind").value === "rtl_tcp" ? "サーバー側欠落：通知なし" : "ソース欠落 " + s.source_overflows}${s.reconnects ? ` · 自動再接続 ${s.reconnects} 回` : ""}${measuredRate(s)}${s.source_info ? ` · ${s.source_info}` : ""}${inputLevel(s)}`
          : s.running
            ? "接続中"
            : "未接続";
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
    // Older backends report only `android`; Android builds are always simple.
    const simpleName =
      platform.name ?? (platform.android ? "JR-CLRR For Android" : null);
    if ((platform.simple ?? platform.android) && simpleName) {
      applySimpleLayout(simpleName);
      addViewMode();
    }
    // Live audio always plays through Web Audio (the app's own player is off).
    nativeAudio = false;
    if (platform.android) {
      addTestSound();
      addRecordingsBrowser();
    }
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
  else if (platform.simple) adoptMacDefaults();
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
