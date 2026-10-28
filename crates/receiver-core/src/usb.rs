//! Built-in RTL-SDR driver (Android): the dongle is driven directly over USB
//! with the vendored pure-Rust `rtlsdr-nusb`, so no separate driver app or
//! RTL-TCP connection is needed.
//!
//! Android apps cannot open USB devices themselves: the app's Java side asks
//! for permission, opens the device and hands its file descriptor over with
//! [`offer_fd`]. A session waiting for a dongle (at start or after the dongle
//! dropped off the bus) reports it through [`wants_device`], so the Java side
//! knows when to look for one.
use crate::{diag::note, source::Read, Complex32};
use rtlsdr_nusb::{Device, MaybeFuture, RxStream};
use serde::{Deserialize, Serialize};
use std::{
    os::fd::{BorrowedFd, OwnedFd},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    pub sample_rate: u32,
    pub center_hz: u32,
    /// Manual LNA + mixer gain in dB; `None` lets the tuner choose.
    pub gain_db: Option<f64>,
    #[serde(default)]
    pub ppm: i32,
    /// Keep trying to reopen the dongle this long after it stops or drops
    /// off the bus (0: fail at once).
    #[serde(default = "default_reconnect_seconds")]
    pub reconnect_seconds: u32,
}
fn default_reconnect_seconds() -> u32 {
    60
}
impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if !(225_001..=300_000).contains(&self.sample_rate)
            && !(900_001..=3_200_000).contains(&self.sample_rate)
        {
            return Err(
                "USB: サンプルレートは 225001–300000 または 900001–3200000 S/s を指定してください"
                    .into(),
            );
        }
        if self.center_hz == 0 {
            return Err("USB: 中心周波数は 0 より大きい値を指定してください".into());
        }
        if self
            .gain_db
            .is_some_and(|g| !g.is_finite() || !(0.0..=52.5).contains(&g))
            || !(-488..=488).contains(&self.ppm)
        {
            return Err(
                "USB: ゲインは 0–52.5 dB（空欄で自動）、周波数補正は −488–488 ppm を指定してください"
                    .into(),
            );
        }
        Ok(())
    }
}

/// What the Java side knows about the dongle besides its descriptor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Identity {
    /// Let the driver read the USB strings itself.
    #[default]
    Unknown,
    /// Android reported manufacturer "RTLSDRBlog", product "Blog V4".
    BlogV4,
    /// Android read the strings and they are not a Blog V4.
    Other,
}
impl Identity {
    pub fn from_code(code: i32) -> Self {
        match code {
            1 => Self::BlogV4,
            2 => Self::Other,
            _ => Self::Unknown,
        }
    }
}
/// The descriptor most recently handed over by the Java side.
static OFFERED: Mutex<Option<(OwnedFd, Identity)>> = Mutex::new(None);
/// A session is waiting for a dongle.
static WANTED: AtomicBool = AtomicBool::new(false);

/// Hands over an opened USB device. The descriptor is duplicated, so the
/// caller keeps (and may later close) its own.
pub fn offer_fd(fd: i32, identity: Identity) {
    if fd < 0 {
        return;
    }
    // SAFETY: the caller passes a descriptor that is open for this call.
    let owned = unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned();
    match owned {
        Ok(owned) => {
            note(format!(
                "ドライバー: ドングルを受け取りました（{identity:?}）"
            ));
            *OFFERED.lock().unwrap() = Some((owned, identity));
        }
        Err(e) => note(format!("ドライバー: 受け取った接続を複製できません（{e}）")),
    }
}
/// Whether a session is waiting for [`offer_fd`].
pub fn wants_device() -> bool {
    WANTED.load(Ordering::Relaxed)
}
fn take_offered() -> Option<(OwnedFd, Identity)> {
    OFFERED.lock().unwrap().take()
}

/// How long `open` waits for the Java side (permission dialog included).
const OPEN_WAIT: Duration = Duration::from_secs(30);
const RETRY_EVERY: Duration = Duration::from_millis(200);
/// No samples for this long: the dongle stalled.
const STALL: Duration = Duration::from_secs(5);
/// A dongle that just came back on USB is left alone this long before it is
/// initialised (it dropped off during initialisation 15 ms after returning).
const SETTLE: Duration = Duration::from_secs(1);

/// Opening from a descriptor needs Linux usbfs (Android is Linux).
#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn open_device(
    _config: &Config,
    (_fd, _identity): (OwnedFd, Identity),
) -> Result<(Device, RxStream), String> {
    Err("USB: 内蔵ドライバーは Android 版でのみ使えます".into())
}
#[cfg(any(target_os = "linux", target_os = "android"))]
fn open_device(
    config: &Config,
    (fd, identity): (OwnedFd, Identity),
) -> Result<(Device, RxStream), String> {
    let gain = config.gain_db.map_or(rtlsdr_nusb::GainConfig::Auto, |g| {
        rtlsdr_nusb::GainConfig::Manual(g as f32)
    });
    let device = Device::builder()
        .frequency_hz(config.center_hz.into())
        .sample_rate_hz(config.sample_rate)
        .gain(gain)
        .correction_ppm(config.ppm);
    let (manufacturer, product) = match identity {
        Identity::BlogV4 => (Some("RTLSDRBlog".into()), Some("Blog V4".into())),
        Identity::Other => (Some(String::new()), Some(String::new())),
        Identity::Unknown => (None, None),
    };
    let device = device
        .open_fd_with_strings(fd, manufacturer, product)
        .wait()
        .map_err(|e| format!("USB: ドングルを初期化できません ({e})"))?;
    let mut rx = device
        .rx_stream()
        .map_err(|e| format!("USB: 受信を開始できません ({e})"))?;
    rx.start()
        .wait()
        .map_err(|e| format!("USB: 受信を開始できません ({e})"))?;
    note(format!(
        "ドライバー: 受信を開始しました（{} · {} S/s · {} Hz · ゲイン {}）",
        describe(&device),
        config.sample_rate,
        config.center_hz,
        config
            .gain_db
            .map_or("自動".to_string(), |g| format!("{g} dB"))
    ));
    Ok((device, rx))
}

/// Tuner and V4 handling of an opened dongle, for the app's status line.
fn describe(device: &Device) -> String {
    let info = device.info();
    let tuner = match info.tuner {
        rtlsdr_nusb::TunerKind::R820T => "R820T",
        rtlsdr_nusb::TunerKind::R828D => "R828D",
        _ => "不明なチューナー",
    };
    if info.rtl_sdr_blog_v4 {
        format!("{tuner} · RTL-SDR Blog V4")
    } else if info.tuner == rtlsdr_nusb::TunerKind::R828D {
        // Not a Blog V4: driven as a plain R828D with its own 16 MHz crystal.
        format!("{tuner}（16 MHz・V4 以外）")
    } else {
        tuner.into()
    }
}

pub struct Usb {
    config: Config,
    /// Tuner and V4 handling, e.g. "R828D · RTL-SDR Blog V4".
    info: String,
    /// USB transfer errors the driver recovered from, over all connections.
    transfer_errors: u64,
    /// Kept for the stream's lifetime (shut down on drop).
    device: Option<Device>,
    rx: Option<RxStream>,
    scratch: Vec<rtlsdr_nusb::Complex32>,
    dropped: u64,
    last_data: Instant,
    lost_at: Option<Instant>,
    cause: String,
    pub failed_attempts: u32,
    pub reconnects: u64,
    /// Transfer errors already noted, and when (notes at most once a second).
    noted_errors: (u64, Instant),
}
impl Usb {
    pub fn open(config: &Config, stop: &AtomicBool) -> Result<Self, String> {
        config.validate()?;
        // A descriptor offered before this session is stale (or is the one
        // the Java side just opened for us); prefer a fresh one when it comes.
        WANTED.store(true, Ordering::Relaxed);
        note("ドライバー: ドングルを待っています");
        let started = Instant::now();
        let fd = loop {
            if stop.load(Ordering::Relaxed) {
                WANTED.store(false, Ordering::Relaxed);
                return Err("USB: 接続を中止しました".into());
            }
            if let Some(fd) = take_offered() {
                break fd;
            }
            if started.elapsed() >= OPEN_WAIT {
                WANTED.store(false, Ordering::Relaxed);
                note("ドライバー: 30 秒待ってもドングルを受け取れませんでした");
                return Err(
                    "USB: 30 秒待ちましたが、ドングルを使えませんでした。挿さっているか、USB の使用を許可したかを確認してください"
                        .into(),
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        WANTED.store(false, Ordering::Relaxed);
        let (device, rx) =
            open_device(config, fd).inspect_err(|e| note(format!("ドライバー: {e}")))?;
        Ok(Self {
            config: config.clone(),
            info: describe(&device),
            transfer_errors: 0,
            device: Some(device),
            rx: Some(rx),
            scratch: Vec::new(),
            dropped: 0,
            last_data: Instant::now(),
            lost_at: None,
            cause: String::new(),
            failed_attempts: 0,
            reconnects: 0,
            noted_errors: (0, Instant::now()),
        })
    }
    pub fn reconnecting(&self) -> bool {
        self.rx.is_none()
    }
    /// For the status line: tuner, V4 handling and recovered USB errors.
    pub fn info(&self) -> String {
        let errors =
            self.transfer_errors + self.rx.as_ref().map_or(0, |rx| rx.stats().transfer_errors);
        if errors == 0 {
            self.info.clone()
        } else {
            format!("{} · USB 転送エラー {errors} 回（自動回復）", self.info)
        }
    }
    fn lose(&mut self, cause: String) -> Result<Read, String> {
        if let Some(rx) = self.rx.take() {
            let stats = rx.stats();
            self.transfer_errors += stats.transfer_errors;
            note(format!(
                "ドライバー: 接続が切れました: {cause}（回復した転送エラー {} 回、最後のエラー {:?}）",
                stats.transfer_errors, stats.last_transfer_error
            ));
        }
        self.device = None;
        if self.config.reconnect_seconds == 0 {
            return Err(cause);
        }
        self.lost_at = Some(Instant::now());
        self.cause = cause;
        self.failed_attempts = 0;
        // A descriptor of the old connection must not be reused.
        take_offered();
        WANTED.store(true, Ordering::Relaxed);
        Ok(Read::Discontinuity)
    }
    pub fn read(&mut self, output: &mut [Complex32], stop: &AtomicBool) -> Result<Read, String> {
        if let Some(rx) = self.rx.as_mut() {
            self.scratch
                .resize(output.len(), rtlsdr_nusb::Complex32::default());
            return match rx
                .read(&mut self.scratch, Some(Duration::from_millis(100)))
                .wait()
            {
                Ok(0) if self.last_data.elapsed() >= STALL => {
                    self.lose("USB: ドングルから 5 秒間データが届きません".into())
                }
                Ok(0) => Ok(Read::Timeout),
                Ok(n) => {
                    self.last_data = Instant::now();
                    // The stream drops whole USB buffers when it falls behind;
                    // let the pipelines resynchronise instead of splicing.
                    let stats = rx.stats();
                    if stats.transfer_errors != self.noted_errors.0
                        && self.noted_errors.1.elapsed() >= Duration::from_secs(1)
                    {
                        note(format!(
                            "ドライバー: USB 転送エラー（この接続で {} 回、最後 {:?}）・受信は継続",
                            stats.transfer_errors, stats.last_transfer_error
                        ));
                        self.noted_errors = (stats.transfer_errors, Instant::now());
                    }
                    let dropped = stats.buffers_dropped;
                    if dropped != self.dropped {
                        self.dropped = dropped;
                        return Ok(Read::Discontinuity);
                    }
                    for (out, s) in output.iter_mut().zip(&self.scratch[..n]) {
                        *out = Complex32 {
                            real: s.re,
                            imaginary: s.im,
                        };
                    }
                    Ok(Read::Samples(n))
                }
                Err(e) => self.lose(format!(
                    "USB: ドングルとの通信が続けて失敗しました（{e}）。ケーブル・ハブ・電源を確認してください"
                )),
            };
        }
        let limit = Duration::from_secs(self.config.reconnect_seconds.into());
        if self.lost_at.is_some_and(|t| t.elapsed() >= limit) {
            WANTED.store(false, Ordering::Relaxed);
            note(format!(
                "ドライバー: {} 秒待ってもドングルが戻りませんでした",
                limit.as_secs()
            ));
            return Err(format!(
                "{}（{} 秒間待ちましたが、ドングルが戻りませんでした。挿し直してから受信を開始してください）",
                self.cause,
                limit.as_secs()
            ));
        }
        if stop.load(Ordering::Relaxed) {
            return Ok(Read::Timeout);
        }
        let Some(fd) = take_offered() else {
            std::thread::sleep(RETRY_EVERY);
            return Ok(Read::Timeout);
        };
        let settled = Instant::now();
        while settled.elapsed() < SETTLE && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(50));
        }
        match open_device(&self.config, fd) {
            Ok((device, rx)) => {
                WANTED.store(false, Ordering::Relaxed);
                self.info = describe(&device);
                self.device = Some(device);
                self.rx = Some(rx);
                self.dropped = 0;
                self.last_data = Instant::now();
                self.lost_at = None;
                self.failed_attempts = 0;
                self.reconnects += 1;
                self.noted_errors = (0, Instant::now());
                note("ドライバー: 再接続しました");
            }
            Err(e) => {
                note(format!("ドライバー: 再接続に失敗しました: {e}"));
                self.failed_attempts += 1
            }
        }
        Ok(Read::Timeout)
    }
}
impl Drop for Usb {
    fn drop(&mut self) {
        WANTED.store(false, Ordering::Relaxed);
        note("ドライバー: ドングルを閉じます");
        if let Some(rx) = self.rx.take() {
            let _ = rx.close().wait();
        }
        if let Some(mut device) = self.device.take() {
            let _ = device.shutdown().wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config {
            sample_rate: 240_000,
            center_hz: 352_603_125,
            gain_db: Some(20.0),
            ppm: 0,
            reconnect_seconds: 60,
        }
    }

    #[test]
    fn settings_follow_the_rtl2832u_limits() {
        assert!(config().validate().is_ok());
        for rate in [225_000, 300_001, 900_000, 3_200_001] {
            assert!(Config {
                sample_rate: rate,
                ..config()
            }
            .validate()
            .is_err());
        }
        for gain in [-0.1, 52.6, f64::NAN] {
            assert!(Config {
                gain_db: Some(gain),
                ..config()
            }
            .validate()
            .is_err());
        }
        assert!(Config {
            gain_db: None,
            ..config()
        }
        .validate()
        .is_ok());
        assert!(Config {
            ppm: 489,
            ..config()
        }
        .validate()
        .is_err());
    }

    #[test]
    fn open_waits_for_a_device_and_can_be_cancelled() {
        let stop = AtomicBool::new(true);
        let error = Usb::open(&config(), &stop).err().unwrap();
        assert!(error.contains("中止"), "{error}");
        assert!(!wants_device());
    }

    #[test]
    fn offered_descriptors_are_duplicated_and_taken_once() {
        let file = std::fs::File::open("/dev/null").unwrap();
        use std::os::fd::AsRawFd;
        offer_fd(file.as_raw_fd(), Identity::BlogV4);
        drop(file);
        // The duplicate stays valid after the caller closed its own.
        let (fd, identity) = take_offered().unwrap();
        assert_eq!(identity, Identity::BlogV4);
        assert!(fd.try_clone().is_ok());
        assert!(take_offered().is_none());
        offer_fd(-1, Identity::Unknown);
        assert!(take_offered().is_none());
    }
}
