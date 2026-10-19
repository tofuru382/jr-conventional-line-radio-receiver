//! Native rtl_tcp input. Control words are big-endian; samples are interleaved CU8.
//! The protocol has no command acknowledgements or sample-loss counters.
use crate::{source::Read, Complex32};
use serde::{Deserialize, Serialize};
use std::{
    io::{ErrorKind, Read as _, Write},
    net::{IpAddr, SocketAddr, TcpStream},
    sync::atomic::{AtomicBool, Ordering},
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
}
fn default_reconnect_seconds() -> u32 {
    RECONNECT_LIMIT.as_secs() as u32
}
impl Config {
    pub fn validate(&self) -> Result<SocketAddr, String> {
        let ip: IpAddr = self
            .host
            .parse()
            .map_err(|_| "RTL-TCP: IPv4 / IPv6 アドレスを入力してください")?;
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
        Ok(SocketAddr::new(ip, self.port))
    }
}

pub struct RtlTcp {
    stream: TcpStream,
    bytes: Vec<u8>,
    filled: usize,
    last_data: Instant,
    pub tuner_type: u32,
    pub gain_count: u32,
}
impl RtlTcp {
    pub fn open(config: &Config, stop: &AtomicBool) -> Result<Self, String> {
        let address = config.validate()?;
        let mut stream =
            TcpStream::connect_timeout(&address, Duration::from_secs(3)).map_err(|e| {
                format!(
                    "RTL-TCP: {address} に接続できません。サーバーとポートを確認してください ({e})"
                )
            })?;
        stream
            .set_read_timeout(Some(Duration::from_millis(100)))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_secs(1)))
            .map_err(|e| e.to_string())?;
        stream.set_nodelay(true).map_err(|e| e.to_string())?;
        let mut header = [0; 12];
        let mut filled = 0;
        let deadline = Instant::now() + Duration::from_secs(3);
        while filled < header.len() {
            if stop.load(Ordering::Relaxed) {
                return Err("RTL-TCP: 接続を中止しました".into());
            }
            if Instant::now() >= deadline {
                return Err(
                    "RTL-TCP: ヘッダーが届きません。他のクライアントが接続中か確認してください"
                        .into(),
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
        let mut input = Self {
            stream,
            bytes: Vec::new(),
            filled: 0,
            last_data: Instant::now(),
            tuner_type: u32::from_be_bytes(header[4..8].try_into().unwrap()),
            gain_count: u32::from_be_bytes(header[8..12].try_into().unwrap()),
        };
        input.command(0x02, config.sample_rate)?;
        input.command(0x01, config.center_hz)?;
        input.command(0x05, config.ppm as u32)?;
        input.command(0x03, u32::from(config.gain_db.is_some()))?;
        if let Some(gain) = config.gain_db {
            input.command(0x04, (gain * 10.0).round() as i32 as u32)?;
        }
        Ok(input)
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
        })
    }
    pub fn reconnecting(&self) -> bool {
        self.inner.is_none()
    }
    pub fn read(&mut self, output: &mut [Complex32], stop: &AtomicBool) -> Result<Read, String> {
        if let Some(inner) = self.inner.as_mut() {
            match inner.read(output) {
                Ok(read) => return Ok(read),
                Err(e) if self.config.reconnect_seconds == 0 => return Err(e),
                Err(e) => {
                    self.inner = None;
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
        match RtlTcp::open(&self.config, stop) {
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
        c.host = "::1".into();
        assert!(c.validate().is_ok());
        c.port = 0;
        assert!(c.validate().is_err());
    }
}
