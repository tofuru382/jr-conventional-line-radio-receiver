//! Application-owned versioned JSON: reusable channel sets + receive profiles.
use crate::{
    data::Format,
    pipeline::{validate_channels, ChannelConfig},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    fs::File,
    io::Read,
    path::Path,
    sync::OnceLock,
};

const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReceivePreset {
    pub id: String,
    pub label: String,
    pub region: Region,
    pub center_hz: f64,
    pub sample_rate: f64,
    pub note: String,
    pub channel_notes: BTreeMap<String, String>,
    /// Known lines per channel id (reference only; may be incomplete).
    #[serde(default)]
    pub channel_lines: BTreeMap<String, String>,
    pub channels: Vec<ChannelConfig>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    schema_version: u32,
    channel_sets: Vec<ChannelSet>,
    profiles: Vec<Profile>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChannelSet {
    id: String,
    region: Region,
    direction: Direction,
    channels: Vec<Channel>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Region {
    East,
    Central,
    West,
}
#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Direction {
    Downlink,
    Uplink,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Channel {
    frequency_hz: u64,
    label: String,
    service: Service,
    #[serde(default)]
    lines: String,
    note: String,
}
#[derive(Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
enum Service {
    Voice,
    Data,
    Common,
    Unassigned,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    id: String,
    label: String,
    channel_set: String,
    services: Vec<Service>,
    capture: Capture,
    note: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Capture {
    center_hz: u64,
    sample_rate: u32,
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// Validate the entire configuration before exposing any profiles.
pub fn parse(text: &str) -> Result<Vec<ReceivePreset>, String> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if value.get("schema_version").and_then(|v| v.as_u64()) != Some(1) {
        return Err("受信設定の schema_version は 1 を指定してください".into());
    }
    let config: Configuration = serde_json::from_value(value).map_err(|e| e.to_string())?;
    debug_assert_eq!(config.schema_version, 1);
    if config.channel_sets.is_empty() || config.profiles.is_empty() {
        return Err("チャンネル定義と受信プロファイルを 1 件以上指定してください".into());
    }
    let mut sets = HashSet::new();
    for set in &config.channel_sets {
        if !valid_id(&set.id) || !sets.insert(&set.id) {
            return Err("チャンネル定義の ID が不正または重複しています".into());
        }
        if set.direction != Direction::Downlink {
            return Err(format!("{}: 移動局からの上り復号は未対応です", set.id));
        }
        if set.channels.is_empty() || set.channels.len() > 64 {
            return Err("各チャンネル定義は 1–64 波にしてください".into());
        }
        let mut frequencies = HashSet::new();
        for channel in &set.channels {
            if channel.frequency_hz == 0
                || channel.frequency_hz > 100_000_000_000
                || !frequencies.insert(channel.frequency_hz)
                || channel.label.trim().is_empty()
            {
                return Err(format!(
                    "{}: 周波数の範囲・重複、またはラベルを確認してください",
                    set.id
                ));
            }
        }
    }
    let mut ids = HashSet::new();
    let mut result = Vec::new();
    for profile in config.profiles {
        if !valid_id(&profile.id)
            || !ids.insert(profile.id.clone())
            || profile.label.trim().is_empty()
        {
            return Err("プロファイルの ID・ラベルを確認してください".into());
        }
        let set = config
            .channel_sets
            .iter()
            .find(|s| s.id == profile.channel_set)
            .ok_or_else(|| format!("{}: 存在しないチャンネル定義です", profile.id))?;
        let services: HashSet<_> = profile.services.iter().collect();
        if services.is_empty() || services.len() != profile.services.len() {
            return Err("受信対象の種別が空または重複しています".into());
        }
        let selected: Vec<_> = set
            .channels
            .iter()
            .filter(|c| services.contains(&c.service))
            .collect();
        let mut channels: Vec<_> = selected
            .iter()
            .map(|c| ChannelConfig {
                id: c.frequency_hz.to_string(),
                label: c.label.clone(),
                frequency_hz: c.frequency_hz as f64,
                format: if c.service == Service::Common {
                    Format::Common
                } else {
                    Format::Slotted
                },
            })
            .collect();
        channels.sort_by(|a, b| a.frequency_hz.total_cmp(&b.frequency_hz));
        let center = profile.capture.center_hz as f64;
        let rate = profile.capture.sample_rate as f64;
        if profile.capture.center_hz == 0
            || profile.capture.center_hz > 100_000_000_000
            || !(9600..=20_000_000).contains(&profile.capture.sample_rate)
        {
            return Err("中心周波数またはサンプルレートが範囲外です".into());
        }
        validate_channels(&channels, rate, center).map_err(|e| format!("{}: {e}", profile.id))?;
        result.push(ReceivePreset {
            id: profile.id,
            label: profile.label,
            region: set.region,
            center_hz: center,
            sample_rate: rate,
            note: profile.note,
            channel_notes: selected
                .iter()
                .filter(|c| !c.note.is_empty())
                .map(|c| (c.frequency_hz.to_string(), c.note.clone()))
                .collect(),
            channel_lines: selected
                .into_iter()
                .filter(|c| !c.lines.is_empty())
                .map(|c| (c.frequency_hz.to_string(), c.lines.clone()))
                .collect(),
            channels,
        });
    }
    Ok(result)
}
pub fn catalog() -> Result<&'static [ReceivePreset], String> {
    static CATALOG: OnceLock<Result<Vec<ReceivePreset>, String>> = OnceLock::new();
    CATALOG
        .get_or_init(|| parse(include_str!("../../../ui/radio-presets.json")))
        .as_ref()
        .map(Vec::as_slice)
        .map_err(Clone::clone)
}
/// Read an external catalog without modifying either it or the bundled one.
/// Validate the whole document before publishing any of its profiles.
pub fn load_file(path: &Path) -> Result<Vec<ReceivePreset>, String> {
    let read = || -> Result<Vec<ReceivePreset>, String> {
        let metadata = path.metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file() {
            return Err("通常の JSON ファイルを選択してください。".into());
        }
        if metadata.len() > MAX_FILE_BYTES {
            return Err("プリセットファイルは 2 MiB 以下にしてください。".into());
        }
        let mut bytes = Vec::new();
        File::open(path)
            .map_err(|e| e.to_string())?
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err("プリセットファイルは 2 MiB 以下にしてください。".into());
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| "UTF-8 の JSON ファイルを選択してください。".to_string())?;
        parse(text.strip_prefix('\u{feff}').unwrap_or(text))
    };
    read().map_err(|e| format!("{}：{e}", path.display()))
}
pub fn resolve(id: &str) -> Result<ReceivePreset, String> {
    catalog()?
        .iter()
        .find(|p| p.id == id)
        .cloned()
        .ok_or_else(|| {
            format!("Unknown receive preset: {id}. Use presets --list to list supported IDs.")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn configuration() -> Value {
        serde_json::from_str(include_str!("../../../ui/radio-presets.json")).unwrap()
    }

    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "jr-presets-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn external_catalog_is_read_fresh_without_changing_the_bundled_catalog() {
        let temp = Temp::new();
        let path = temp.0.join("路線.json");
        let mut config = configuration();
        config["profiles"] = json!([config["profiles"][1].clone()]);
        config["profiles"][0]["id"] = json!("custom-line");
        config["profiles"][0]["label"] = json!("外部の路線");
        let bytes = format!("\u{feff}{config}");
        std::fs::write(&path, &bytes).unwrap();
        let external = load_file(&path).unwrap();
        assert_eq!(external.len(), 1);
        assert_eq!(external[0].id, "custom-line");
        assert_eq!(external[0].channels.len(), 3);
        assert_eq!(external[0].center_hz, 352625000.0);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), bytes);
        config["profiles"][0]["label"] = json!("更新した路線");
        std::fs::write(&path, config.to_string()).unwrap();
        assert_eq!(load_file(&path).unwrap()[0].label, "更新した路線");
        assert_eq!(catalog().unwrap().len(), 3);
        assert!(resolve("custom-line").is_err());
    }

    #[test]
    fn external_catalog_rejects_missing_non_file_oversized_and_invalid_content() {
        let temp = Temp::new();
        let path = temp.0.join("invalid.json");
        assert!(load_file(&temp.0).is_err());
        assert!(load_file(&path).unwrap_err().contains("invalid.json"));
        let mut invalid = configuration();
        invalid["profiles"][1]["capture"]["center_hz"] = json!(1);
        for bytes in [
            b"not json".to_vec(),
            vec![0xff, 0xfe],
            invalid.to_string().into_bytes(),
            vec![b' '; MAX_FILE_BYTES as usize + 1],
        ] {
            std::fs::write(&path, &bytes).unwrap();
            assert!(load_file(&path).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
    }

    #[test]
    fn bundled_profiles_cover_exactly_the_posfie_company_allocations() {
        // Posfie / odaQshirei, JR allocation chart dated 2026-08-28.
        // West has no assignments at 352.61875, 352.62500, 352.63125 MHz;
        // Central uses only D4's two voice frequencies and one data frequency.
        let all: Vec<u64> = (0..22).map(|i| 352537500 + i * 6250).collect();
        let west: Vec<u64> = all
            .iter()
            .copied()
            .filter(|f| ![352618750, 352625000, 352631250].contains(f))
            .collect();
        assert_eq!(
            catalog()
                .unwrap()
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>(),
            vec!["east", "central", "west"]
        );
        for (id, label, expected, center) in [
            ("east", "JR 東日本（22波）", all, 352603125.0),
            (
                "central",
                "JR 東海（3波）",
                vec![352600000, 352637500, 352650000],
                352625000.0,
            ),
            ("west", "JR 西日本（19波）", west, 352603125.0),
        ] {
            let p = resolve(id).unwrap();
            assert_eq!(p.label, label);
            assert_eq!(
                p.channels
                    .iter()
                    .map(|c| c.frequency_hz as u64)
                    .collect::<Vec<_>>(),
                expected,
                "{id}"
            );
            assert_eq!(p.center_hz, center);
            assert_eq!(p.sample_rate, 240000.0);
            assert!(p
                .channels
                .iter()
                .all(|c| c.id == (c.frequency_hz as u64).to_string()));
            validate_channels(&p.channels, p.sample_rate, p.center_hz).unwrap();
            for c in &p.channels {
                if c.format == Format::Common {
                    assert_eq!(c.label, "旅客一斉情報");
                } else {
                    let (channel, usage) = c.label.split_once(' ').unwrap();
                    assert!(channel
                        .strip_prefix('D')
                        .and_then(|n| n.parse::<u8>().ok())
                        .is_some_and(|n| (1..=7).contains(&n)));
                    assert!(["音声第1波", "音声第2波", "データ波"].contains(&usage));
                }
            }
        }
        let east = resolve("east").unwrap();
        assert_eq!(east.channels[14].format, Format::Common);
        // Reference line names follow the channel group (D1–D7).
        assert_eq!(east.channel_lines["352625000"], "全線区共通");
        let west = resolve("west").unwrap();
        for id in ["352568750", "352606250", "352662500"] {
            assert_eq!(
                west.channel_lines[id],
                "大阪環状線・桜島線・梅田貨物線・奈良線"
            );
        }
        assert_eq!(east.channel_notes.len(), 8);
        assert_eq!(
            east.channel_notes["352612500"],
            "変則割当：中央本線（甲府～小淵沢）D6 音声第1波"
        );
        assert_eq!(
            east.channel_notes["352618750"],
            "変則割当：小海線 D4 音声第1波"
        );
        assert_eq!(
            east.channel_notes["352581250"],
            "変則割当：陸羽東線 D3 音声第2波 / 大船渡線 D7 音声第2波"
        );
        for removed in [
            "east-wide",
            "east-data",
            "west-assigned",
            "central-assigned",
            "missing",
            "jr-east-mobile",
        ] {
            assert!(resolve(removed).is_err());
        }
        // Channel sets themselves contain no unassigned padding, and every
        // assigned entry is selected by its company's single profile.
        let config: Configuration = serde_json::from_value(configuration()).unwrap();
        for set in &config.channel_sets {
            assert!(set
                .channels
                .iter()
                .all(|c| c.service != Service::Unassigned));
            let profile = config
                .profiles
                .iter()
                .find(|p| p.channel_set == set.id)
                .unwrap();
            assert_eq!(
                set.channels.len(),
                resolve(&profile.id).unwrap().channels.len()
            );
        }
    }

    #[test]
    fn configuration_rejects_ambiguous_and_unresolved_references() {
        for mutation in [
            "set_id",
            "profile_id",
            "frequency",
            "missing_set",
            "empty_selection",
            "duplicate_service",
        ] {
            let mut c = configuration();
            match mutation {
                "set_id" => c["channel_sets"][1]["id"] = c["channel_sets"][0]["id"].clone(),
                "profile_id" => c["profiles"][1]["id"] = c["profiles"][0]["id"].clone(),
                "frequency" => {
                    c["channel_sets"][0]["channels"][1]["frequency_hz"] =
                        c["channel_sets"][0]["channels"][0]["frequency_hz"].clone()
                }
                "missing_set" => c["profiles"][0]["channel_set"] = json!("missing"),
                "empty_selection" => c["profiles"][0]["services"] = json!(["unassigned"]),
                "duplicate_service" => c["profiles"][0]["services"] = json!(["data", "data"]),
                _ => unreachable!(),
            }
            assert!(parse(&c.to_string()).is_err(), "{mutation}");
        }
    }

    #[test]
    fn schema_errors_and_unsupported_direction_fail_before_exposing_any_profile() {
        for (pointer, value) in [
            ("/schema_version", json!(2)),
            ("/profiles/0/capture/sample_rate", json!(240000.5)),
            ("/profiles/0/capture/sample_rate", json!(9600)),
            ("/profiles/0/capture/center_hz", json!(1)),
            ("/channel_sets/0/direction", json!("uplink")),
            ("/channel_sets/0/region", json!("unknown")),
            ("/channel_sets/0/channels/0/service", json!("unknown")),
            ("/channel_sets/0/channels/0/frequency_hz", json!(0)),
            (
                "/channel_sets/0/channels/0/frequency_hz",
                json!(100_000_000_001u64),
            ),
            ("/channel_sets/0/channels/0/label", json!(" ")),
            ("/profiles/0/id", json!("bad id")),
            ("/profiles", json!([])),
        ] {
            let mut c = configuration();
            *c.pointer_mut(pointer).unwrap() = value;
            assert!(parse(&c.to_string()).is_err(), "{pointer}");
        }
        for pointer in [
            "",
            "/profiles/0",
            "/profiles/0/capture",
            "/channel_sets/0",
            "/channel_sets/0/channels/0",
        ] {
            let mut c = configuration();
            c.pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("typo".into(), json!(true));
            assert!(parse(&c.to_string()).is_err(), "{pointer}");
        }
        let mut c = configuration();
        c.as_object_mut().unwrap().remove("schema_version");
        assert!(parse(&c.to_string()).is_err());
    }
}
