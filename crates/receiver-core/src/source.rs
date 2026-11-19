//! SoapySDR source using the installed, stable 0.8 C ABI. No device-specific backend.
#[cfg(feature = "soapy")]
use crate::Complex32;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Device {
    pub label: String,
    pub args: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RadioConfig {
    pub args: String,
    pub sample_rate: f64,
    pub center_hz: f64,
    /// Manual gain in dB, or `None` for the device's automatic gain.
    pub gain_db: Option<f64>,
    pub antenna: String,
    pub channel: usize,
    /// Tuner frequency correction (ppm), where the device supports it.
    #[serde(default)]
    pub ppm: f64,
    /// Stream arguments (SoapySDR key=value list). Empty: chosen for the
    /// driver (RTL-SDR: USB buffers of about 40 ms instead of the driver's
    /// half second at 240 kS/s, which arrives in bursts).
    #[serde(default)]
    pub stream_args: String,
    /// Bias tee (antenna power) through the driver's `biastee` setting.
    #[serde(default)]
    pub bias_tee: bool,
    /// Keep trying to reopen the device this long after it stops delivering
    /// samples or fails (0: fail at once).
    #[serde(default = "default_reconnect_seconds")]
    pub reconnect_seconds: u32,
}
fn default_reconnect_seconds() -> u32 {
    60
}
#[derive(Debug)]
pub enum Read {
    Samples(usize),
    /// Samples that follow a gap in the stream (lost samples before them).
    SamplesAfterGap(usize),
    Timeout,
    Discontinuity,
}

fn deserialize_some<'de, T, D>(deserializer: D) -> Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    T::deserialize(deserializer).map(Some)
}
/// A tuning change while receiving; absent fields stay as they are.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Control {
    /// Absent: unchanged. `null`: automatic gain. A number: manual gain (dB).
    #[serde(default, deserialize_with = "deserialize_some")]
    pub gain_db: Option<Option<f64>>,
    #[serde(default)]
    pub ppm: Option<i32>,
    #[serde(default)]
    pub bias_tee: Option<bool>,
}
impl Control {
    pub fn validate(&self) -> Result<(), String> {
        if self
            .gain_db
            .flatten()
            .is_some_and(|g| !g.is_finite() || !(-10.0..=100.0).contains(&g))
            || self.ppm.is_some_and(|p| !(-1000..=1000).contains(&p))
        {
            return Err(
                "ゲインは −10–100 dB、周波数補正は −1000–1000 ppm を指定してください".into(),
            );
        }
        Ok(())
    }
    /// Later changes win field by field.
    pub fn merge(&mut self, later: Control) {
        if later.gain_db.is_some() {
            self.gain_db = later.gain_db;
        }
        if later.ppm.is_some() {
            self.ppm = later.ppm;
        }
        if later.bias_tee.is_some() {
            self.bias_tee = later.bias_tee;
        }
    }
    pub fn update_rtl_tcp(&self, config: &mut crate::rtl_tcp::Config) {
        if let Some(gain) = self.gain_db {
            config.gain_db = gain;
        }
        if let Some(ppm) = self.ppm {
            config.ppm = ppm;
        }
        if let Some(bias) = self.bias_tee {
            config.bias_tee = bias;
        }
    }
    pub fn update_radio(&self, config: &mut RadioConfig) {
        if let Some(gain) = self.gain_db {
            config.gain_db = gain;
        }
        if let Some(ppm) = self.ppm {
            config.ppm = ppm.into();
        }
        if let Some(bias) = self.bias_tee {
            config.bias_tee = bias;
        }
    }
}
#[cfg(feature = "soapy")]
mod soapy {
    use super::*;
    use std::{
        ffi::{c_char, c_int, c_long, c_void, CStr, CString},
        ptr,
    };
    #[repr(C)]
    struct Kwargs {
        size: usize,
        keys: *mut *mut c_char,
        vals: *mut *mut c_char,
    }
    // All device and stream pointers remain on the creating acquisition thread.
    // CF32 is two contiguous IEEE binary32 values, matching Complex32's repr(C).
    extern "C" {
        fn SoapySDRDevice_enumerateStrArgs(args: *const c_char, len: *mut usize) -> *mut Kwargs;
        fn SoapySDRKwargs_toString(args: *const Kwargs) -> *mut c_char;
        fn SoapySDRKwargs_get(args: *const Kwargs, key: *const c_char) -> *const c_char;
        fn SoapySDRKwargsList_clear(args: *mut Kwargs, len: usize);
        fn SoapySDR_free(p: *mut c_void);
        fn SoapySDRDevice_lastError() -> *const c_char;
        fn SoapySDRDevice_lastStatus() -> c_int;
        fn SoapySDRDevice_makeStrArgs(args: *const c_char) -> *mut c_void;
        fn SoapySDRDevice_unmake(dev: *mut c_void) -> c_int;
        fn SoapySDRDevice_getNumChannels(dev: *const c_void, direction: c_int) -> usize;
        fn SoapySDRDevice_setSampleRate(
            dev: *mut c_void,
            direction: c_int,
            ch: usize,
            rate: f64,
        ) -> c_int;
        fn SoapySDRDevice_getSampleRate(dev: *const c_void, direction: c_int, ch: usize) -> f64;
        fn SoapySDRDevice_setFrequency(
            dev: *mut c_void,
            direction: c_int,
            ch: usize,
            freq: f64,
            args: *const Kwargs,
        ) -> c_int;
        fn SoapySDRDevice_getFrequency(dev: *const c_void, direction: c_int, ch: usize) -> f64;
        fn SoapySDRDevice_setGain(
            dev: *mut c_void,
            direction: c_int,
            ch: usize,
            gain: f64,
        ) -> c_int;
        fn SoapySDRDevice_setGainMode(
            dev: *mut c_void,
            direction: c_int,
            ch: usize,
            automatic: bool,
        ) -> c_int;
        fn SoapySDRDevice_setFrequencyCorrection(
            dev: *mut c_void,
            direction: c_int,
            ch: usize,
            value: f64,
        ) -> c_int;
        fn SoapySDRDevice_writeSetting(
            dev: *mut c_void,
            key: *const c_char,
            value: *const c_char,
        ) -> c_int;
        fn SoapySDRDevice_getDriverKey(dev: *const c_void) -> *mut c_char;
        fn SoapySDRKwargs_fromString(markup: *const c_char) -> Kwargs;
        fn SoapySDRKwargs_clear(args: *mut Kwargs);
        fn SoapySDRDevice_setAntenna(
            dev: *mut c_void,
            direction: c_int,
            ch: usize,
            antenna: *const c_char,
        ) -> c_int;
        fn SoapySDRDevice_setupStream(
            dev: *mut c_void,
            direction: c_int,
            format: *const c_char,
            channels: *const usize,
            count: usize,
            args: *const Kwargs,
        ) -> *mut c_void;
        fn SoapySDRDevice_activateStream(
            dev: *mut c_void,
            stream: *mut c_void,
            flags: c_int,
            time: i64,
            elems: usize,
        ) -> c_int;
        fn SoapySDRDevice_deactivateStream(
            dev: *mut c_void,
            stream: *mut c_void,
            flags: c_int,
            time: i64,
        ) -> c_int;
        fn SoapySDRDevice_closeStream(dev: *mut c_void, stream: *mut c_void) -> c_int;
        fn SoapySDRDevice_readStream(
            dev: *mut c_void,
            stream: *mut c_void,
            buffs: *const *mut c_void,
            count: usize,
            flags: *mut c_int,
            time: *mut i64,
            timeout: c_long,
        ) -> c_int;
    }
    fn string(p: *const c_char) -> String {
        if p.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
        }
    }
    fn error(action: &str) -> String {
        format!(
            "{action}: {}",
            string(unsafe { SoapySDRDevice_lastError() })
        )
    }
    fn cstring(s: &str) -> Result<CString, String> {
        CString::new(s).map_err(|_| "Device argument contains NUL".into())
    }
    fn check(code: c_int, action: &str) -> Result<(), String> {
        if code == 0 {
            Ok(())
        } else {
            Err(error(action))
        }
    }
    pub fn enumerate() -> Result<Vec<Device>, String> {
        unsafe {
            let mut count = 0;
            let list = SoapySDRDevice_enumerateStrArgs(c"".as_ptr(), &mut count);
            if list.is_null() {
                return if SoapySDRDevice_lastStatus() == 0 {
                    Ok(Vec::new())
                } else {
                    Err(error("SDR enumeration failed"))
                };
            }
            let mut devices = Vec::new();
            for i in 0..count {
                let kwargs = list.add(i);
                let raw = SoapySDRKwargs_toString(kwargs);
                let args = string(raw);
                SoapySDR_free(raw.cast());
                let label = string(SoapySDRKwargs_get(kwargs, c"label".as_ptr()));
                devices.push(Device {
                    label: if label.is_empty() {
                        args.clone()
                    } else {
                        label
                    },
                    args,
                });
            }
            SoapySDRKwargsList_clear(list, count);
            Ok(devices)
        }
    }
    // readStream results (SoapySDR/Errors.h) and flags (Constants.h).
    const TIMEOUT: c_int = -1;
    const CORRUPTION: c_int = -3;
    const OVERFLOW: c_int = -4;
    const TIME_ERROR: c_int = -6;
    const HAS_TIME: c_int = 1 << 2;
    const END_ABRUPT: c_int = 1 << 3;
    /// No samples for this long: the device is treated as lost.
    const STALL: std::time::Duration = std::time::Duration::from_secs(5);

    pub struct Sdr {
        dev: *mut c_void,
        stream: *mut c_void,
        pub sample_rate: f64,
        pub center_hz: f64,
        active: bool,
        channel: usize,
        driver: String,
        last_data: std::time::Instant,
        // Hardware time (ns) expected for the next sample, when the driver
        // reports timestamps: a jump beyond it is a gap in the stream.
        next_time: Option<i64>,
    }
    pub(super) fn stream_args(driver: &str, configured: &str, sample_rate: f64) -> String {
        if !configured.trim().is_empty() || driver != "rtlsdr" {
            return configured.trim().into();
        }
        // SoapyRTLSDR's 256 KiB buffers last half a second at 240 kS/s and
        // arrive in bursts; about 40 ms (a multiple of 16 KiB) keeps the
        // stream steady and the latency low.
        let bytes = (sample_rate * 2.0 * 0.04 / 16384.0).round().max(1.0) as usize * 16384;
        format!("bufflen={bytes},buffers=32")
    }
    impl Sdr {
        pub fn open(c: &RadioConfig) -> Result<Self, String> {
            if !c.sample_rate.is_finite()
                || !(9600.0..=20_000_000.0).contains(&c.sample_rate)
                || !c.center_hz.is_finite()
                || c.center_hz <= 0.0
                || c.gain_db.is_some_and(|g| !g.is_finite())
            {
                return Err("Invalid radio configuration".into());
            }
            let args = cstring(&c.args)?;
            unsafe {
                let dev = SoapySDRDevice_makeStrArgs(args.as_ptr());
                if dev.is_null() {
                    return Err(error("Cannot open SDR"));
                }
                let key = SoapySDRDevice_getDriverKey(dev);
                let driver = string(key);
                SoapySDR_free(key.cast());
                let mut s = Self {
                    dev,
                    stream: ptr::null_mut(),
                    sample_rate: 0.0,
                    center_hz: 0.0,
                    active: false,
                    channel: c.channel,
                    driver,
                    last_data: std::time::Instant::now(),
                    next_time: None,
                };
                if c.channel >= SoapySDRDevice_getNumChannels(dev, 1) {
                    return Err("RX channel does not exist".into());
                }
                check(
                    SoapySDRDevice_setSampleRate(dev, 1, c.channel, c.sample_rate),
                    "Set sample rate",
                )?;
                if c.ppm != 0.0 {
                    if !c.ppm.is_finite() {
                        return Err("Invalid frequency correction".into());
                    }
                    check(
                        SoapySDRDevice_setFrequencyCorrection(dev, 1, c.channel, c.ppm),
                        "Set frequency correction",
                    )?;
                }
                check(
                    SoapySDRDevice_setFrequency(dev, 1, c.channel, c.center_hz, ptr::null()),
                    "Tune SDR",
                )?;
                s.set_gain(c.gain_db)?;
                if c.bias_tee {
                    s.set_bias_tee(true)?;
                }
                if !c.antenna.is_empty() {
                    let a = cstring(&c.antenna)?;
                    check(
                        SoapySDRDevice_setAntenna(dev, 1, c.channel, a.as_ptr()),
                        "Set antenna",
                    )?;
                }
                s.sample_rate = SoapySDRDevice_getSampleRate(dev, 1, c.channel);
                s.center_hz = SoapySDRDevice_getFrequency(dev, 1, c.channel);
                let markup = cstring(&stream_args(&s.driver, &c.stream_args, s.sample_rate))?;
                let mut kwargs = SoapySDRKwargs_fromString(markup.as_ptr());
                s.stream =
                    SoapySDRDevice_setupStream(dev, 1, c"CF32".as_ptr(), &c.channel, 1, &kwargs);
                SoapySDRKwargs_clear(&mut kwargs);
                if s.stream.is_null() {
                    return Err(error("Create CF32 RX stream"));
                }
                check(
                    SoapySDRDevice_activateStream(dev, s.stream, 0, 0, 0),
                    "Activate RX stream",
                )?;
                s.active = true;
                Ok(s)
            }
        }
        /// Manual gain, or the device's automatic gain for `None`. Devices
        /// without an automatic mode keep their default gain then.
        pub fn set_gain(&mut self, gain_db: Option<f64>) -> Result<(), String> {
            unsafe {
                // Not every device has a gain mode; that is not an error.
                SoapySDRDevice_setGainMode(self.dev, 1, self.channel, gain_db.is_none());
                if let Some(g) = gain_db {
                    if !g.is_finite() {
                        return Err("Invalid gain".into());
                    }
                    check(
                        SoapySDRDevice_setGain(self.dev, 1, self.channel, g),
                        "Set gain",
                    )?;
                }
            }
            Ok(())
        }
        pub fn set_bias_tee(&mut self, on: bool) -> Result<(), String> {
            let value = if on { c"true" } else { c"false" };
            check(
                unsafe {
                    SoapySDRDevice_writeSetting(self.dev, c"biastee".as_ptr(), value.as_ptr())
                },
                "Set bias tee",
            )
        }
        /// Applies a tuning change while streaming.
        pub fn apply(&mut self, control: &Control) -> Result<(), String> {
            if let Some(gain) = control.gain_db {
                self.set_gain(gain)?;
            }
            if let Some(ppm) = control.ppm {
                check(
                    unsafe {
                        SoapySDRDevice_setFrequencyCorrection(self.dev, 1, self.channel, ppm.into())
                    },
                    "Set frequency correction",
                )?;
            }
            if let Some(bias) = control.bias_tee {
                self.set_bias_tee(bias)?;
            }
            Ok(())
        }
        pub fn read(&mut self, buf: &mut [Complex32]) -> Result<Read, String> {
            let buffers = [buf.as_mut_ptr().cast()];
            let mut flags = 0;
            let mut time = 0;
            let count = unsafe {
                SoapySDRDevice_readStream(
                    self.dev,
                    self.stream,
                    buffers.as_ptr(),
                    buf.len(),
                    &mut flags,
                    &mut time,
                    100_000,
                )
            };
            match count {
                TIMEOUT | 0 => {
                    if self.last_data.elapsed() >= STALL {
                        return Err(format!(
                            "SoapySDR: {} から 5 秒間データが届きません。接続と電源を確認してください",
                            if self.driver.is_empty() { "SDR" } else { &self.driver }
                        ));
                    }
                    Ok(Read::Timeout)
                }
                // Samples were lost or damaged; the stream itself goes on.
                OVERFLOW | CORRUPTION | TIME_ERROR => {
                    self.next_time = None;
                    Ok(Read::Discontinuity)
                }
                n if n > 0 && n as usize <= buf.len() => {
                    self.last_data = std::time::Instant::now();
                    let n = n as usize;
                    let mut gap = flags & END_ABRUPT != 0;
                    if flags & HAS_TIME != 0 {
                        // More than half a block later than expected.
                        let tolerance = (n as f64 / self.sample_rate * 0.5e9) as i64;
                        gap |= self.next_time.is_some_and(|t| time - t > tolerance);
                        self.next_time =
                            Some(time + (n as f64 / self.sample_rate * 1e9).round() as i64);
                    }
                    // Keep the samples; only what came before them was lost.
                    Ok(if gap {
                        Read::SamplesAfterGap(n)
                    } else {
                        Read::Samples(n)
                    })
                }
                _ => Err(format!(
                    "SDR read failed ({count}): {}",
                    error("readStream")
                )),
            }
        }
    }
    impl Drop for Sdr {
        fn drop(&mut self) {
            unsafe {
                if self.active {
                    SoapySDRDevice_deactivateStream(self.dev, self.stream, 0, 0);
                }
                if !self.stream.is_null() {
                    SoapySDRDevice_closeStream(self.dev, self.stream);
                }
                SoapySDRDevice_unmake(self.dev);
            }
        }
    }
}
#[cfg(feature = "soapy")]
pub use soapy::{enumerate, Sdr};

/// SoapySDR input that reopens its device when it fails or stops delivering
/// samples, as the RTL-TCP and USB inputs do: the lost span is reported once
/// as a discontinuity.
#[cfg(feature = "soapy")]
pub struct Reconnecting {
    config: RadioConfig,
    inner: Option<Sdr>,
    lost_at: Option<std::time::Instant>,
    next_attempt: std::time::Instant,
    cause: String,
    pub failed_attempts: u32,
    pub reconnects: u64,
    pub sample_rate: f64,
    pub center_hz: f64,
}
#[cfg(feature = "soapy")]
impl Reconnecting {
    pub fn open(config: &RadioConfig) -> Result<Self, String> {
        let inner = Sdr::open(config)?;
        Ok(Self {
            sample_rate: inner.sample_rate,
            center_hz: inner.center_hz,
            inner: Some(inner),
            config: config.clone(),
            lost_at: None,
            next_attempt: std::time::Instant::now(),
            cause: String::new(),
            failed_attempts: 0,
            reconnects: 0,
        })
    }
    pub fn reconnecting(&self) -> bool {
        self.inner.is_none()
    }
    pub fn apply(&mut self, control: &Control) -> Result<(), String> {
        control.update_radio(&mut self.config);
        match self.inner.as_mut() {
            Some(inner) => inner.apply(control),
            None => Ok(()),
        }
    }
    pub fn read(
        &mut self,
        buf: &mut [Complex32],
        stop: &std::sync::atomic::AtomicBool,
    ) -> Result<Read, String> {
        use std::time::{Duration, Instant};
        if let Some(inner) = self.inner.as_mut() {
            return match inner.read(buf) {
                Ok(read) => Ok(read),
                Err(e) if self.config.reconnect_seconds == 0 => Err(e),
                Err(e) => {
                    crate::diag::note(format!("SoapySDR: {e}（開き直します）"));
                    self.inner = None;
                    self.lost_at = Some(Instant::now());
                    self.next_attempt = Instant::now() + Duration::from_millis(500);
                    self.cause = e;
                    self.failed_attempts = 0;
                    Ok(Read::Discontinuity)
                }
            };
        }
        let limit = Duration::from_secs(self.config.reconnect_seconds.into());
        if self.lost_at.is_some_and(|t| t.elapsed() >= limit) {
            return Err(format!(
                "{}（{} 秒間開き直そうとしましたが、使えませんでした）",
                self.cause,
                limit.as_secs()
            ));
        }
        if Instant::now() < self.next_attempt || stop.load(std::sync::atomic::Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(50));
            return Ok(Read::Timeout);
        }
        match Sdr::open(&self.config) {
            Ok(inner) if inner.sample_rate == self.sample_rate => {
                self.inner = Some(inner);
                self.lost_at = None;
                self.failed_attempts = 0;
                self.reconnects += 1;
                crate::diag::note("SoapySDR: デバイスを開き直しました");
            }
            Ok(_) => {
                return Err("SoapySDR: 開き直したデバイスのサンプルレートが変わりました".into());
            }
            Err(_) => {
                self.failed_attempts += 1;
                self.next_attempt = Instant::now() + Duration::from_secs(2);
            }
        }
        Ok(Read::Timeout)
    }
}
#[cfg(not(feature = "soapy"))]
pub fn enumerate() -> Result<Vec<Device>, String> {
    Err("SoapySDR was disabled at build time".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn control_distinguishes_automatic_gain_from_no_change() {
        let auto: Control = serde_json::from_str(r#"{"gain_db":null}"#).unwrap();
        assert_eq!(auto.gain_db, Some(None));
        let unchanged: Control = serde_json::from_str(r#"{"ppm":3}"#).unwrap();
        assert_eq!((unchanged.gain_db, unchanged.ppm), (None, Some(3)));
        let mut pending = auto;
        pending.merge(serde_json::from_str(r#"{"gain_db":20.5,"bias_tee":true}"#).unwrap());
        assert_eq!(pending.gain_db, Some(Some(20.5)));
        assert_eq!(pending.bias_tee, Some(true));
        assert!(Control {
            ppm: Some(5000),
            ..Control::default()
        }
        .validate()
        .is_err());
    }
    #[cfg(feature = "soapy")]
    #[test]
    fn rtl_sdr_streams_get_short_buffers_unless_configured() {
        assert_eq!(
            soapy::stream_args("rtlsdr", "", 240_000.0),
            "bufflen=16384,buffers=32"
        );
        assert_eq!(
            soapy::stream_args("rtlsdr", "", 1_024_000.0),
            "bufflen=81920,buffers=32"
        );
        assert_eq!(
            soapy::stream_args("rtlsdr", "bufflen=65536", 240_000.0),
            "bufflen=65536"
        );
        assert_eq!(soapy::stream_args("hackrf", "", 2e6), "");
    }
}
