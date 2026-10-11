// Browser preview projection of the same JSON configuration embedded by Rust.
// Native operation uses the fully validated Rust catalog through Tauri IPC.
export function buildCatalog(config) {
  if (config.schema_version !== 1)
    throw new Error("受信設定の schema_version は 1 を指定してください");
  return config.profiles.map((profile) => {
    const set = config.channel_sets.find(
      (set) => set.id === profile.channel_set,
    );
    if (!set || set.direction !== "downlink")
      throw new Error("未対応のチャンネル定義です");
    const selected = set.channels.filter((c) =>
      profile.services.includes(c.service),
    );
    return {
      id: profile.id,
      label: profile.label,
      region: set.region,
      ...profile.capture,
      note: profile.note,
      channel_notes: Object.fromEntries(
        selected
          .filter((c) => c.note)
          .map((c) => [String(c.frequency_hz), c.note]),
      ),
      channel_lines: Object.fromEntries(
        selected
          .filter((c) => c.lines)
          .map((c) => [String(c.frequency_hz), c.lines]),
      ),
      channels: selected
        .map((c) => ({
          id: String(c.frequency_hz),
          label: c.label,
          frequency_hz: c.frequency_hz,
          format: c.service === "common" ? "common" : "slotted",
        }))
        .sort((a, b) => a.frequency_hz - b.frequency_hz),
    };
  });
}
