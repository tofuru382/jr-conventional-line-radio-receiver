//! Native rtl_tcp input. Control words are big-endian; samples are interleaved CU8.
//! The protocol has no command acknowledgements or sample-loss counters.
use crate::{
    source::{Control, Read},
    Complex32,
};
use serde::{Deserialize, Serialize};
use std::{
    io::{ErrorKind, Read as _, Write},
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub sample_rate: u32,
    pub center_hz: u32,
    pub gain_db: Option<f64>,
    #[serde(default)]
    pub ppm: i32,
    /// Keep trying to reconnect this long after the server stops or drops
    /// the connection (0: fail at once).
    #[serde(default = "default_reconnect_seconds")]
    pub reconnect_seconds: u32,
    /// Power an active antenna or LNA through the coaxial cable (rtl_tcp
    /// command 0x0e; servers without it ignore the command).
    #[serde(default)]
    pub bias_tee: bool,
}
fn default_reconnect_seconds() -> u32 {
    RECONNECT_LIMIT.as_secs() as u32
}
/// rtl_tcp's tuner type in its header (librtlsdr's `rtlsdr_tuner`).
pub fn tuner_name(tuner_type: u32) -> &'static str {
    match tuner_type {
        1 => "E4000",
        2 => "FC0012",
        3 => "FC0013",
        4 => "FC2580",
        5 => "R820T",
        6 => "R828D",
        _ => "不明なチューナー",
    }
}
/// What a connection test found at the server.
#[derive(Clone, Debug, Serialize)]
pub struct Probe {
    pub address: String,
    pub tuner: String,
    pub gain_count: u32,
}
impl Config {
    /// Checks the settings without touching the network. The host is an IPv4
    /// or IPv6 address (an IPv6 scope such as `%wlan0` is allowed) or a host
    /// name such as `raspberrypi.local`; names are resolved when connecting.
    pub fn validate(&self) -> Result<(), String> {
        let host = self.host.trim();
        if host.is_empty()
            || host.len() > 253
            || !host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '%' | '_'))
        {
            return Err(
                "RTL-TCP: IPv4 / IPv6 アドレスかホスト名（例: raspberrypi.local）を入力してください"
                    .into(),
            );
        }
        if self.port == 0 || self.center_hz == 0 {
            return Err("RTL-TCP: ポートと中心周波数は 0 より大きい値を指定してください".into());
        }
        // librtlsdr rejects the gap between these two ranges.
        if !(225001..=300000).contains(&self.sample_rate)
            && !(900001..=3200000).contains(&self.sample_rate)
        {
            return Err("RTL-TCP: サンプルレートは 225001–300000 または 900001–3200000 S/s を指定してください".into());
        }
        if self
            .gain_db
            .is_some_and(|g| !g.is_finite() || !(-10.0..=100.0).contains(&g))
            || !(-1000..=1000).contains(&self.ppm)
        {
            return Err(
                "RTL-TCP: ゲインは −10–100 dB、周波数補正は −1000–1000 ppm を指定してください"
                    .into(),
            );
        }
        Ok(())
    }
}

/// Resolves the host (a blocking name lookup for names) and connects to the
/// first address that answers within the timeout, while `stop` can abandon
/// the attempt at any moment: the lookup and connection run on a helper
/// thread, which finishes on its own after the caller has given up.
fn connect(config: &Config, stop: &AtomicBool) -> Result<(TcpStream, SocketAddr), String> {
    let host = config.host.trim().to_string();
    let port = config.port;
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("rtl-tcp-connect".into())
        .spawn(move || {
            let result = (host.as_str(), port)
                .to_socket_addrs()
                .map_err(|e| format!("RTL-TCP: {host} の名前を解決できません ({e})"))
                .and_then(|addresses| {
                    let mut last = format!("RTL-TCP: {host} のアドレスが見つかりません");
                    for address in addresses {
                        match TcpStream::connect_timeout(&address, Duration::from_secs(3)) {
                            Ok(stream) => return Ok((stream, address)),
                            Err(e) => {
                                last = format!(
                                    "RTL-TCP: {address} に接続できません。サーバーとポートを確認してください ({e})"
                                )
                            }
                        }
                    }
                    Err(last)
                });
            let _ = tx.send(result);
        })
        .map_err(|e| format!("RTL-TCP: 接続を開始できません ({e})"))?;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err("RTL-TCP: 接続を中止しました".into());
        }
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(result) => return result,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("RTL-TCP: 接続処理が異常終了しました".into())
            }
        }
    }
}

/// Connection test: connects, reads the server header and disconnects,
/// without configuring the tuner or streaming.
pub fn probe(config: &Config, stop: &AtomicBool) -> Result<Probe, String> {
    config.validate()?;
    let (mut stream, address) = connect(config, stop)?;
    let header = read_header(&mut stream, stop)?;
    let tuner_type = u32::from_be_bytes(header[4..8].try_into().unwrap());
    Ok(Probe {
        address: address.to_string(),
        tuner: tuner_name(tuner_type).into(),
        gain_count: u32::from_be_bytes(header[8..12].try_into().unwrap()),
    })
}

fn read_header(stream: &mut TcpStream, stop: &AtomicBool) -> Result<[u8; 12], String> {
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .map_err(|e| e.to_string())?;
    let mut header = [0; 12];
    let mut filled = 0;
    let deadline = Instant::now() + Duration::from_secs(3);
    while filled < header.len() {
        if stop.load(Ordering::Relaxed) {
            return Err("RTL-TCP: 接続を中止しました".into());
        }
        if Instant::now() >= deadline {
            return Err(
                "RTL-TCP: ヘッダーが届きません。他のクライアントが接続中か確認してください".into(),
            );
        }
        match stream.read(&mut header[filled..]) {
            Ok(0) => return Err("RTL-TCP: ヘッダー受信中に接続が閉じられました".into()),
            Ok(n) => filled += n,
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::TimedOut | ErrorKind::WouldBlock | ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(format!("RTL-TCP: ヘッダーを受信できません ({e})")),
        }
    }
    if &header[..4] != b"RTL0" {
        return Err(
            "RTL-TCP: RTL0 ヘッダーではありません。rtl_tcp のポートを指定してください".into(),
        );
    }
    Ok(header)
}

// rtl_tcp's automatic mode hands gain to the tuner's AGC, which drives the
// 8-bit ADC into clipping on this wideband multi-carrier input (measured: 6.6 %
// of samples clipped, worse than the highest manual gain) and loses frame sync.
// An empty gain therefore selects manual gain stepped down from the top of the
// tuner's table while the ADC clips.
// Light clipping must last this many 50 ms windows before the gain steps.
const LIGHT_CLIPPING_WINDOWS: u32 = 10;
// JR-CLRR: the gain also steps back up. A receiver on a train passes strong
// stations and then needs the gain again for the weak ones. Alternating
// between clipping and not would cost frames on every step, and every change
// is I2C traffic to a dongle that is streaming (dongles that stopped
// answering became more frequent near stations when rises came after 6 s).
// So it only rises after this many windows (20 s) without a single clipped
// sample, only as far as the largest sample seen in them leaves headroom
// for, and each rise that ends in clipping doubles the wait (up to 16 times,
// about 5 minutes; a calm stretch of ten waits halves it again).
const RECOVERY_WINDOWS: u32 = 400;
const RECOVERY_BACKOFF: u32 = 16;
// The largest sample after a rise must stay below this share of full scale.
const RECOVERY_PEAK: f64 = 0.8 * 255.0;
// JR-CLRR: shared with the built-in USB driver (usb.rs), whose tuner AGC
// clips the same way; there the gains come from [`GAINS_TENTH_DB`].
pub(crate) struct AutoGain {
    pub(crate) index: u32,
    // Samples still to discard: a new gain reaches the stream only after the
    // server's USB buffer (256 KiB) and the network drain.
    holdoff: u64,
    pub(crate) settle: u64,
    window: u64,
    seen: u64,
    clipped: u64,
    // Consecutive windows with light clipping; a brief burst does not step.
    strikes: u32,
    top: u32,
    // Consecutive windows without clipping and their largest sample (0–255).
    quiet: u32,
    peak: u32,
    // Clean windows a rise needs, the calm windows towards halving that, and
    // whether the last step was a rise.
    wait: u32,
    calm: u32,
    raised: bool,
}
/// rtl_tcp's (librtlsdr's) R820T/R828D gain table, 0.1 dB steps; rtl_tcp's
/// gain-by-index command selects from it.
pub(crate) const GAINS_TENTH_DB: [u16; 29] = [
    0, 9, 14, 27, 37, 77, 87, 125, 144, 157, 166, 197, 207, 229, 254, 280, 297, 328, 338, 364, 372,
    386, 402, 421, 434, 439, 445, 480, 496,
];
impl AutoGain {
    pub(crate) fn new(gain_count: u32, sample_rate: u32) -> Option<Self> {
        let index = gain_count.checked_sub(1)?;
        let settle = 131072 + u64::from(sample_rate) / 5;
        Some(Self {
            index,
            holdoff: settle,
            settle,
            window: (u64::from(sample_rate) / 20).max(1),
            seen: 0,
            clipped: 0,
            strikes: 0,
            top: index,
            quiet: 0,
            peak: 0,
            wait: RECOVERY_WINDOWS,
            calm: 0,
            raised: false,
        })
    }
    /// Counts clipped components in `bytes` (interleaved CU8) and returns the
    /// next gain index once a full window clipped too often, or once the
    /// input stayed clean long enough to raise the gain again.
    fn observe(&mut self, bytes: &[u8]) -> Option<u32> {
        self.observe_with(bytes.len() / 2, |start| {
            let (mut clipped, mut peak) = (0, 0);
            for &b in &bytes[start * 2..] {
                let level = (i32::from(b) * 2 - 255).unsigned_abs();
                clipped += u64::from(level == 255);
                peak = peak.max(level);
            }
            (clipped, peak)
        })
    }
    /// Like [`Self::observe`] for `samples` samples of another format:
    /// `measure(start)` returns the number of clipped I/Q components from
    /// sample `start` and the largest component (0–255, 255 = full scale).
    pub(crate) fn observe_with(
        &mut self,
        samples: usize,
        measure: impl FnOnce(usize) -> (u64, u32),
    ) -> Option<u32> {
        let mut samples = samples as u64;
        let mut start = 0;
        if self.holdoff > 0 {
            let skip = self.holdoff.min(samples);
            self.holdoff -= skip;
            samples -= skip;
            start = skip as usize;
        }
        if samples == 0 {
            return None;
        }
        let (clipped, peak) = measure(start);
        self.seen += samples;
        self.clipped += clipped;
        self.peak = self.peak.max(peak);
        if self.seen < self.window {
            return None;
        }
        // Clipped I/Q components per 1000 samples.
        let rate = self.clipped * 1000 / self.seen;
        let clean = self.clipped == 0;
        self.seen = 0;
        self.clipped = 0;
        if !clean {
            self.quiet = 0;
            self.peak = 0;
            self.calm = 0;
        }
        let step = match rate {
            0 => {
                self.strikes = 0;
                return if clean { self.recover() } else { None };
            }
            1..=9 => {
                self.strikes += 1;
                if self.strikes < LIGHT_CLIPPING_WINDOWS {
                    return None;
                }
                1
            }
            10..=39 => 2,
            _ => 3,
        };
        self.strikes = 0;
        if self.index == 0 {
            return None;
        }
        if self.raised {
            self.raised = false;
            self.wait = (self.wait * 2).min(RECOVERY_WINDOWS * RECOVERY_BACKOFF);
        }
        self.index = self.index.saturating_sub(step);
        self.holdoff = self.settle;
        Some(self.index)
    }
    /// A clean window: raises the gain (up to three steps) once enough of
    /// them passed and the largest sample leaves headroom for it.
    fn recover(&mut self) -> Option<u32> {
        self.calm += 1;
        if self.calm >= self.wait.saturating_mul(10) {
            self.calm = 0;
            self.wait = (self.wait / 2).max(RECOVERY_WINDOWS);
        }
        self.quiet += 1;
        if self.quiet < self.wait {
            return None;
        }
        let peak = f64::from(self.peak.max(1));
        self.quiet = 0;
        self.peak = 0;
        let mut index = self.index;
        while index < self.top
            && index < self.index + 3
            && peak * 10f64.powf(self.rise_db(index + 1) / 20.0) <= RECOVERY_PEAK
        {
            index += 1;
        }
        if index == self.index {
            return None;
        }
        self.index = index;
        self.holdoff = self.settle;
        self.raised = true;
        Some(index)
    }
    /// Gain added by going from the current index to `index`: from the
    /// R820T/R828D table, or a cautious 3 dB a step for another tuner's.
    fn rise_db(&self, index: u32) -> f64 {
        if self.top as usize == GAINS_TENTH_DB.len() - 1 {
            f64::from(GAINS_TENTH_DB[index as usize] - GAINS_TENTH_DB[self.index as usize]) / 10.0
        } else {
            3.0 * f64::from(index - self.index)
        }
    }
}

pub struct RtlTcp {
    stream: TcpStream,
    bytes: Vec<u8>,
    filled: usize,
    last_data: Instant,
    auto_gain: Option<AutoGain>,
    pub tuner_type: u32,
    pub gain_count: u32,
}
impl RtlTcp {
    pub fn open(config: &Config, stop: &AtomicBool) -> Result<Self, String> {
        Self::open_at(config, stop, None)
    }
    /// JR-CLRR: like [`Self::open`], with automatic gain starting no higher
    /// than `gain_index` (where an earlier connection had stepped down to).
    pub fn open_at(
        config: &Config,
        stop: &AtomicBool,
        gain_index: Option<u32>,
    ) -> Result<Self, String> {
        config.validate()?;
        let (mut stream, _) = connect(config, stop)?;
        stream
            .set_write_timeout(Some(Duration::from_secs(1)))
            .map_err(|e| e.to_string())?;
        stream.set_nodelay(true).map_err(|e| e.to_string())?;
        let header = read_header(&mut stream, stop)?;
        let mut input = Self {
            stream,
            bytes: Vec::new(),
            filled: 0,
            last_data: Instant::now(),
            auto_gain: None,
            tuner_type: u32::from_be_bytes(header[4..8].try_into().unwrap()),
            gain_count: u32::from_be_bytes(header[8..12].try_into().unwrap()),
        };
        input.command(0x02, config.sample_rate)?;
        input.command(0x01, config.center_hz)?;
        input.command(0x05, config.ppm as u32)?;
        match config.gain_db {
            Some(gain) => {
                input.command(0x03, 1)?;
                input.command(0x04, (gain * 10.0).round() as i32 as u32)?;
            }
            None => match AutoGain::new(input.gain_count, config.sample_rate) {
                Some(mut auto) => {
                    if let Some(index) = gain_index {
                        auto.index = auto.index.min(index);
                    }
                    input.command(0x03, 1)?;
                    input.command(0x0d, auto.index)?;
                    input.auto_gain = Some(auto);
                }
                // No gain table reported: only the tuner's AGC is available.
                None => input.command(0x03, 0)?,
            },
        }
        if config.bias_tee {
            input.command(0x0e, 1)?;
        }
        Ok(input)
    }
    /// Changes tuning while streaming (rtl_tcp applies each command at once).
    /// The new gain reaches the samples after the server's buffers drain, so
    /// the automatic gain waits before measuring again.
    pub fn apply(&mut self, control: &Control, config: &Config) -> Result<(), String> {
        if let Some(gain) = control.gain_db {
            match gain {
                Some(gain) => {
                    self.auto_gain = None;
                    self.command(0x03, 1)?;
                    self.command(0x04, (gain * 10.0).round() as i32 as u32)?;
                }
                None => match AutoGain::new(self.gain_count, config.sample_rate) {
                    Some(auto) => {
                        self.command(0x03, 1)?;
                        self.command(0x0d, auto.index)?;
                        self.auto_gain = Some(auto);
                    }
                    None => self.command(0x03, 0)?,
                },
            }
        }
        if let Some(ppm) = control.ppm {
            self.command(0x05, ppm as u32)?;
        }
        if let Some(bias) = control.bias_tee {
            self.command(0x0e, u32::from(bias))?;
        }
        Ok(())
    }
    fn command(&mut self, command: u8, value: u32) -> Result<(), String> {
        let mut word = [command, 0, 0, 0, 0];
        word[1..].copy_from_slice(&value.to_be_bytes());
        self.stream
            .write_all(&word)
            .map_err(|e| format!("RTL-TCP: 設定を送信できません ({e})"))
    }
    pub fn read(&mut self, output: &mut [Complex32]) -> Result<Read, String> {
        if output.is_empty() {
            return Err("RTL-TCP: IQ buffer is empty".into());
        }
        let size = output.len() * 2;
        // Keep partial samples across TCP packet boundaries and timeouts.
        if self.filled > size {
            return Err("RTL-TCP: IQ buffer shrank during a partial read".into());
        }
        self.bytes.resize(size, 0);
        let started = Instant::now();
        while self.filled < size {
            if started.elapsed() >= Duration::from_millis(100) {
                return Ok(Read::Timeout);
            }
            match self.stream.read(&mut self.bytes[self.filled..]) {
                Ok(0) => {
                    return Err(
                        "RTL-TCP: サーバーが接続を閉じました。受信を再開してください".into(),
                    )
                }
                Ok(n) => {
                    self.filled += n;
                    self.last_data = Instant::now();
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        ErrorKind::TimedOut | ErrorKind::WouldBlock | ErrorKind::Interrupted
                    ) =>
                {
                    if self.last_data.elapsed() >= Duration::from_secs(5) {
                        return Err("RTL-TCP: IQ データが 5 秒間届きません。サーバーと回線を確認してください".into());
                    }
                    return Ok(Read::Timeout);
                }
                Err(e) => return Err(format!("RTL-TCP: IQ 受信エラー ({e})")),
            }
        }
        if let Some(index) = self
            .auto_gain
            .as_mut()
            .and_then(|g| g.observe(&self.bytes[..size]))
        {
            self.command(0x0d, index)?;
        }
        for (out, pair) in output.iter_mut().zip(self.bytes.as_chunks::<2>().0) {
            *out = Complex32 {
                real: (pair[0] as f32 - 127.5) / 128.0,
                imaginary: (pair[1] as f32 - 127.5) / 128.0,
            };
        }
        self.filled = 0;
        Ok(Read::Samples(output.len()))
    }
}

/// Wait between reconnection attempts.
const RETRY_EVERY: Duration = Duration::from_secs(2);
/// Default: give up (and report the original error) after this long without a server.
pub const RECONNECT_LIMIT: Duration = Duration::from_secs(60);

/// RTL-TCP input that reconnects on its own when the server stops sending or
/// drops the connection (an on-phone driver app can restart or briefly lose
/// its USB dongle). The lost span is reported once as a discontinuity, so the
/// pipelines resynchronise instead of splicing unrelated IQ together.
pub struct Reconnecting {
    config: Config,
    inner: Option<RtlTcp>,
    lost_at: Option<Instant>,
    next_attempt: Instant,
    /// Why the connection was lost (reported if reconnection fails).
    cause: String,
    /// Failed attempts in the current outage.
    pub failed_attempts: u32,
    /// Successful reconnections in this session.
    pub reconnects: u64,
    /// Automatic gain index of the lost connection, to resume from.
    gain_index: Option<u32>,
}
impl Reconnecting {
    pub fn open(config: &Config, stop: &AtomicBool) -> Result<Self, String> {
        Ok(Self {
            inner: Some(RtlTcp::open(config, stop)?),
            config: config.clone(),
            lost_at: None,
            next_attempt: Instant::now(),
            cause: String::new(),
            failed_attempts: 0,
            reconnects: 0,
            gain_index: None,
        })
    }
    pub fn reconnecting(&self) -> bool {
        self.inner.is_none()
    }
    /// For the status line: the server's tuner and the gain in use.
    pub fn info(&self) -> Option<String> {
        let inner = self.inner.as_ref()?;
        let mut info = tuner_name(inner.tuner_type).to_string();
        match (&inner.auto_gain, self.config.gain_db) {
            (Some(auto), _) => {
                if let Some(db) = GAINS_TENTH_DB.get(auto.index as usize) {
                    info += &format!(" · ゲイン 自動 {} dB", f64::from(*db) / 10.0);
                }
            }
            (None, Some(db)) => info += &format!(" · ゲイン {db} dB"),
            (None, None) => info += " · ゲイン チューナー AGC",
        }
        if self.config.bias_tee {
            info += " · Bias-T オン";
        }
        Some(info)
    }
    /// Applies a tuning change now and to every later reconnection.
    pub fn apply(&mut self, control: &Control) -> Result<(), String> {
        control.update_rtl_tcp(&mut self.config);
        if control.gain_db.is_some() {
            // A new connection starts from the new gain, not the old index.
            self.gain_index = None;
        }
        match self.inner.as_mut() {
            Some(inner) => inner.apply(control, &self.config),
            None => Ok(()),
        }
    }
    pub fn read(&mut self, output: &mut [Complex32], stop: &AtomicBool) -> Result<Read, String> {
        if let Some(inner) = self.inner.as_mut() {
            match inner.read(output) {
                Ok(read) => return Ok(read),
                Err(e) if self.config.reconnect_seconds == 0 => return Err(e),
                Err(e) => {
                    self.gain_index = self
                        .inner
                        .take()
                        .and_then(|lost| lost.auto_gain.map(|g| g.index));
                    self.lost_at = Some(Instant::now());
                    self.next_attempt = Instant::now() + Duration::from_millis(500);
                    self.cause = e;
                    self.failed_attempts = 0;
                    return Ok(Read::Discontinuity);
                }
            }
        }
        let limit = Duration::from_secs(self.config.reconnect_seconds.into());
        if self.lost_at.is_some_and(|t| t.elapsed() >= limit) {
            return Err(format!(
                "{}（{} 秒間自動で再接続を試みましたが、つながりませんでした）",
                self.cause,
                limit.as_secs()
            ));
        }
        if Instant::now() < self.next_attempt || stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(50));
            return Ok(Read::Timeout);
        }
        match RtlTcp::open_at(&self.config, stop, self.gain_index) {
            Ok(inner) => {
                self.inner = Some(inner);
                self.lost_at = None;
                self.failed_attempts = 0;
                self.reconnects += 1;
            }
            Err(_) => {
                self.failed_attempts += 1;
                self.next_attempt = Instant::now() + RETRY_EVERY;
            }
        }
        Ok(Read::Timeout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        net::TcpListener,
        sync::{mpsc, Arc},
        thread,
    };
    fn config(port: u16) -> Config {
        Config {
            host: "127.0.0.1".into(),
            port,
            sample_rate: 240000,
            center_hz: 352603125,
            gain_db: Some(19.7),
            ppm: -12,
            reconnect_seconds: 60,
            bias_tee: false,
        }
    }
    fn header(s: &mut TcpStream) {
        s.write_all(b"RTL0\0\0\0\x05\0\0\0\x1d").unwrap();
    }
    #[test]
    fn fragmented_iq_survives_timeouts_and_commands_use_network_byte_order() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (resume, wait) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            header(&mut s);
            let mut commands = [0; 25];
            s.read_exact(&mut commands).unwrap();
            for (word, (op, value)) in commands.as_chunks::<5>().0.iter().zip([
                (2, 240000u32),
                (1, 352603125),
                (5, (-12i32) as u32),
                (3, 1),
                (4, 197),
            ]) {
                assert_eq!(word[0], op);
                assert_eq!(u32::from_be_bytes(word[1..].try_into().unwrap()), value);
            }
            s.write_all(&[0, 255, 128]).unwrap();
            wait.recv_timeout(Duration::from_secs(3)).unwrap();
            s.write_all(&[127]).unwrap();
        });
        let mut input = RtlTcp::open(&config(port), &AtomicBool::new(false)).unwrap();
        assert_eq!((input.tuner_type, input.gain_count), (5, 29));
        let mut iq = [Complex32::default(); 2];
        assert!(matches!(input.read(&mut iq).unwrap(), Read::Timeout));
        resume.send(()).unwrap();
        assert!(matches!(input.read(&mut iq).unwrap(), Read::Samples(2)));
        assert_eq!(
            (iq[0].real, iq[0].imaginary),
            (-127.5 / 128.0, 127.5 / 128.0)
        );
        assert_eq!((iq[1].real, iq[1].imaginary), (0.5 / 128.0, -0.5 / 128.0));
        server.join().unwrap();
        assert!(input.read(&mut iq).unwrap_err().contains("接続を閉じ"));
    }
    #[test]
    fn host_names_probe_and_live_tuning_reach_the_server() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            // A connection test: header only, then the client hangs up.
            let (mut s, _) = listener.accept().unwrap();
            header(&mut s);
            drop(s);
            let (mut s, _) = listener.accept().unwrap();
            header(&mut s);
            let mut setup = [0; 25];
            s.read_exact(&mut setup).unwrap();
            // Manual gain 30.5 dB, ppm 7, bias tee on, sent while streaming.
            let mut live = [0; 20];
            s.read_exact(&mut live).unwrap();
            live
        });
        let mut c = config(port);
        c.host = "localhost".into();
        let probe = probe(&c, &AtomicBool::new(false)).unwrap();
        assert_eq!((probe.tuner.as_str(), probe.gain_count), ("R820T", 29));
        let mut input = Reconnecting::open(&c, &AtomicBool::new(false)).unwrap();
        input
            .apply(&Control {
                gain_db: Some(Some(30.5)),
                ppm: Some(7),
                bias_tee: Some(true),
            })
            .unwrap();
        let live = server.join().unwrap();
        let words: Vec<(u8, u32)> = live
            .as_chunks::<5>()
            .0
            .iter()
            .map(|w| (w[0], u32::from_be_bytes(w[1..].try_into().unwrap())))
            .collect();
        assert_eq!(words, vec![(3, 1), (4, 305), (5, 7), (14, 1)]);
        // Kept for reconnections, and shown in the status line.
        assert_eq!(input.config.gain_db, Some(30.5));
        assert!(input.info().unwrap().contains("Bias-T"));
    }
    #[test]
    fn connecting_can_be_abandoned_at_once() {
        let started = Instant::now();
        let mut c = config(9);
        // Documentation range (TEST-NET-3): nothing answers there.
        c.host = "203.0.113.1".into();
        let error = RtlTcp::open(&c, &AtomicBool::new(true)).err().unwrap();
        assert!(error.contains("中止"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn empty_gain_steps_manual_gain_down_while_the_adc_clips() {
        // 240 kS/s: 50 ms windows of 12000 samples after a 179072-sample settle.
        let mut gain = AutoGain::new(29, 240000).unwrap();
        assert_eq!(gain.index, 28);
        let clean = vec![128u8; 24000];
        let mut heavy = clean.clone();
        heavy[..1000].fill(255);
        // Samples still in flight from the previous gain are ignored.
        let in_flight = vec![255u8; gain.settle as usize * 2];
        assert_eq!(gain.observe(&in_flight), None);
        assert_eq!(gain.holdoff, 0);
        assert_eq!(gain.observe(&heavy), Some(25));
        assert_eq!(gain.holdoff, gain.settle);
        // Light clipping (1 per mille) steps once only after persisting.
        gain.holdoff = 0;
        let mut light = clean.clone();
        light[..12].fill(0);
        for _ in 0..LIGHT_CLIPPING_WINDOWS - 1 {
            assert_eq!(gain.observe(&light), None);
        }
        assert_eq!(gain.observe(&light), Some(24));
        gain.holdoff = 0;
        for _ in 0..LIGHT_CLIPPING_WINDOWS - 1 {
            assert_eq!(gain.observe(&light), None);
        }
        assert_eq!(gain.observe(&clean), None);
        assert_eq!(gain.observe(&light), None);
        // A short clean stretch does not raise the gain; the lowest index
        // stays put.
        for _ in 0..100 {
            assert_eq!(gain.observe(&clean), None);
        }
        gain.index = 1;
        assert_eq!(gain.observe(&heavy), Some(0));
        gain.holdoff = 0;
        assert_eq!(gain.observe(&heavy), None);
        assert!(AutoGain::new(0, 240000).is_none());
    }
    #[test]
    fn automatic_gain_rises_again_only_with_headroom_and_backs_off_after_clipping() {
        // 50 ms windows of 12000 samples; levels are |2 * byte - 255| of 255.
        let window = |byte: u8| vec![byte; 24000];
        let run = |gain: &mut AutoGain, bytes: &[u8], windows: u32| {
            (0..windows).find_map(|_| {
                gain.holdoff = 0;
                gain.observe(bytes)
            })
        };
        let mut gain = AutoGain::new(29, 240000).unwrap();
        gain.index = 20;
        // 37.2 dB with samples up to 25 % of full scale: 6 s later the gain
        // rises three steps (to 42.1 dB, predicting 43 %), never earlier.
        let quiet = window(159);
        assert_eq!(run(&mut gain, &quiet, RECOVERY_WINDOWS - 1), None);
        assert_eq!(gain.observe(&quiet), Some(23));
        assert_eq!(gain.holdoff, gain.settle);
        // Samples up to 75 % leave room for 0.5 dB (index 24 to 25) only, and
        // none at 85 %.
        gain.index = 24;
        assert_eq!(run(&mut gain, &window(223), RECOVERY_WINDOWS), Some(25));
        assert_eq!(run(&mut gain, &window(236), RECOVERY_WINDOWS * 3), None);
        // One clipped sample restarts the wait.
        let mut spike = quiet.clone();
        spike[0] = 255;
        assert_eq!(run(&mut gain, &quiet, RECOVERY_WINDOWS - 1), None);
        assert_eq!(gain.observe(&spike), None);
        assert_eq!(run(&mut gain, &quiet, RECOVERY_WINDOWS - 1), None);
        assert_eq!(gain.observe(&quiet), Some(28));
        // At the top there is nothing to raise.
        assert_eq!(run(&mut gain, &quiet, RECOVERY_WINDOWS * 2), None);
        // A rise that ends in clipping doubles the wait before the next one.
        let mut heavy = quiet.clone();
        heavy[..1000].fill(255);
        gain.index = 20;
        assert_eq!(run(&mut gain, &quiet, RECOVERY_WINDOWS), Some(23));
        assert_eq!(run(&mut gain, &heavy, 1), Some(20));
        assert_eq!(run(&mut gain, &quiet, RECOVERY_WINDOWS * 2 - 1), None);
        assert_eq!(gain.observe(&quiet), Some(23));
        assert_eq!(run(&mut gain, &heavy, 1), Some(20));
        assert_eq!(run(&mut gain, &quiet, RECOVERY_WINDOWS * 4 - 1), None);
        assert_eq!(gain.observe(&quiet), Some(23));
        // Clipping that did not follow a rise does not lengthen the wait, and
        // a long calm stretch shortens it again.
        assert_eq!(run(&mut gain, &heavy, 1), Some(20));
        assert_eq!(run(&mut gain, &heavy, 1), Some(17));
        assert_eq!(gain.wait, RECOVERY_WINDOWS * 8);
        gain.index = 28;
        assert_eq!(run(&mut gain, &quiet, RECOVERY_WINDOWS * 80), None);
        assert_eq!(gain.wait, RECOVERY_WINDOWS * 4);
        // Another tuner's table is unknown: 3 dB a step is assumed.
        let mut other = AutoGain::new(14, 240000).unwrap();
        other.index = 5;
        assert_eq!(run(&mut other, &window(190), RECOVERY_WINDOWS), Some(6));
        // The lowest index rises too.
        other.index = 0;
        assert_eq!(run(&mut other, &quiet, RECOVERY_WINDOWS), Some(3));
    }
    #[test]
    fn empty_gain_selects_the_top_manual_gain_index() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            header(&mut s);
            let mut commands = [0; 25];
            s.read_exact(&mut commands).unwrap();
            let words: Vec<_> = commands
                .as_chunks::<5>()
                .0
                .iter()
                .map(|w| (w[0], u32::from_be_bytes(w[1..].try_into().unwrap())))
                .collect();
            assert_eq!(&words[3..], [(3, 1), (0x0d, 28)]);
        });
        let mut c = config(port);
        c.gain_db = None;
        let input = RtlTcp::open(&c, &AtomicBool::new(false)).unwrap();
        assert_eq!(input.auto_gain.as_ref().map(|g| g.index), Some(28));
        server.join().unwrap();
    }
    #[test]
    fn silent_server_fails_instead_of_receiving_forever() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (release, wait) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            header(&mut s);
            wait.recv_timeout(Duration::from_secs(3)).unwrap();
        });
        let mut input = RtlTcp::open(&config(port), &AtomicBool::new(false)).unwrap();
        input.last_data = Instant::now() - Duration::from_secs(6);
        let error = input.read(&mut [Complex32::default(); 10]).err().unwrap();
        assert!(error.contains("5 秒間"));
        release.send(()).unwrap();
        server.join().unwrap();
    }
    #[test]
    fn dropped_server_is_reconnected_after_one_discontinuity() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            for byte in [10u8, 20] {
                let (mut s, _) = listener.accept().unwrap();
                header(&mut s);
                let mut commands = [0; 25];
                s.read_exact(&mut commands).unwrap();
                s.write_all(&[byte; 4]).unwrap();
                // Closing at once: the driver app restarted or lost USB.
            }
        });
        let stop = AtomicBool::new(false);
        let mut input = Reconnecting::open(&config(port), &stop).unwrap();
        let mut iq = [Complex32::default(); 2];
        let mut reads = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while reads
            .iter()
            .filter(|r| matches!(r, Read::Samples(_)))
            .count()
            < 2
        {
            assert!(Instant::now() < deadline, "no reconnection: {reads:?}");
            let read = input.read(&mut iq, &stop).unwrap();
            if matches!(read, Read::Discontinuity) {
                assert!(input.reconnecting());
            }
            if !matches!(read, Read::Timeout) {
                reads.push(read);
            }
        }
        assert!(matches!(reads[0], Read::Samples(2)));
        assert!(matches!(reads[1], Read::Discontinuity));
        assert!(matches!(reads[2], Read::Samples(2)));
        assert_eq!(iq[0].real, (20.0 - 127.5) / 128.0);
        assert_eq!(input.reconnects, 1);
        assert!(!input.reconnecting());
        server.join().unwrap();
    }
    // JR-CLRR: a reconnection keeps the gain the automatic gain stepped to.
    #[test]
    fn automatic_gain_resumes_where_it_stepped_down_after_a_reconnection() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let command = |s: &mut TcpStream| {
            let mut word = [0; 5];
            s.read_exact(&mut word).unwrap();
            (word[0], u32::from_be_bytes(word[1..].try_into().unwrap()))
        };
        let server = thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            header(&mut s);
            let first: Vec<_> = (0..5).map(|_| command(&mut s)).collect();
            assert_eq!(first[4], (0x0d, 28));
            // Settling samples, then fully clipped 50 ms windows (more than
            // one, as the client reads in 12000-sample blocks).
            s.write_all(&vec![255u8; (179072 + 36000) * 2]).unwrap();
            assert_eq!(command(&mut s), (0x0d, 25));
            drop(s);
            let (mut s, _) = listener.accept().unwrap();
            header(&mut s);
            let second: Vec<_> = (0..5).map(|_| command(&mut s)).collect();
            assert_eq!(second[4], (0x0d, 25));
            s.write_all(&[128; 4]).unwrap();
        });
        let stop = AtomicBool::new(false);
        let config = Config {
            gain_db: None,
            ..config(port)
        };
        let mut input = Reconnecting::open(&config, &stop).unwrap();
        assert_eq!(input.info().as_deref(), Some("R820T · ゲイン 自動 49.6 dB"));
        let mut iq = vec![Complex32::default(); 12000];
        let deadline = Instant::now() + Duration::from_secs(10);
        while input.reconnects == 0 || input.reconnecting() {
            assert!(Instant::now() < deadline, "no reconnection");
            input.read(&mut iq, &stop).unwrap();
        }
        assert_eq!(input.info().as_deref(), Some("R820T · ゲイン 自動 43.9 dB"));
        server.join().unwrap();
    }
    #[test]
    fn reconnection_gives_up_with_the_original_cause() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            header(&mut s);
            let mut commands = [0; 25];
            s.read_exact(&mut commands).unwrap();
            // The listener closes with the thread: nothing to reconnect to.
        });
        let stop = AtomicBool::new(false);
        let mut input = Reconnecting::open(&config(port), &stop).unwrap();
        server.join().unwrap();
        let mut iq = [Complex32::default(); 2];
        let deadline = Instant::now() + Duration::from_secs(10);
        while !input.reconnecting() {
            assert!(Instant::now() < deadline);
            input.read(&mut iq, &stop).unwrap();
        }
        input.lost_at = Some(Instant::now() - RECONNECT_LIMIT);
        let error = input.read(&mut iq, &stop).unwrap_err();
        assert!(error.contains("接続を閉じ"), "{error}");
        assert!(error.contains("再接続"), "{error}");
    }
    #[test]
    fn header_wait_can_be_cancelled_and_wrong_protocol_is_rejected() {
        let timeout = Duration::from_secs(20);
        for cancel in [true, false] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let stop = Arc::new(AtomicBool::new(false));
            let signal = stop.clone();
            let server = thread::spawn(move || {
                let (mut s, _) = listener.accept().unwrap();
                s.set_read_timeout(Some(timeout)).unwrap();
                if cancel {
                    signal.store(true, Ordering::Relaxed);
                } else {
                    s.write_all(b"HTTP/1.1 200 ").unwrap();
                }
                let mut byte = [0];
                // Rejecting a protocol may close with unread bytes, causing
                // a reset instead of an orderly EOF. Both mean it closed.
                match s.read(&mut byte) {
                    Ok(0) => {}
                    Err(e) if e.kind() == ErrorKind::ConnectionReset => {}
                    result => panic!("expected the client to close its connection, got {result:?}"),
                }
            });
            let (opened, wait) = mpsc::channel();
            let client = thread::spawn(move || {
                let error = RtlTcp::open(&config(port), &stop).err().unwrap();
                opened.send(error).unwrap();
            });
            let error = wait
                .recv_timeout(timeout)
                .expect("RTL-TCP header wait did not finish");
            // Ignoring cancellation would return the missing-header error,
            // which must still fail even on a slow or heavily loaded runner.
            assert!(error.contains(if cancel { "中止" } else { "RTL0" }));
            client.join().unwrap();
            server.join().unwrap();
        }
    }
    #[test]
    fn unsupported_rates_and_invalid_addresses_are_rejected_before_connecting() {
        let mut c = config(1234);
        for rate in [0, 9600, 225000, 300001, 900000, 3200001] {
            c.sample_rate = rate;
            assert!(c.validate().is_err());
        }
        for rate in [225001, 240000, 300000, 900001, 3200000] {
            c.sample_rate = rate;
            assert!(c.validate().is_ok());
        }
        c.host = "bad host".into();
        assert!(c.validate().is_err());
        c.host = "".into();
        assert!(c.validate().is_err());
        for host in [
            "::1",
            "raspberrypi.local",
            "fe80::1%wlan0",
            "rtl-tcp.example.org",
        ] {
            c.host = host.into();
            assert!(c.validate().is_ok(), "{host}");
        }
        c.host = "::1".into();
        c.port = 0;
        assert!(c.validate().is_err());
    }
}
