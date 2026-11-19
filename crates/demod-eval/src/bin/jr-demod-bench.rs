//! Fixed benchmark suite over the production receive pipeline (development
//! tool). Prints one Markdown table: frame acceptance, undetected errors and
//! receiver CPU time per condition, for comparing receiver changes.
//!
//!   jr-demod-bench [--features all|none|NAME,…|-NAME,…] [--seeds N] [--frames N]
//!                  [--dsp KEY=VALUE,…] [--only SUBSTRING] [--json]
use jr_demod_eval::{run, Condition, Path, Report, Traffic};
use std::error::Error;

struct Case {
    name: &'static str,
    traffic: Traffic,
    es_n0_db: f64,
    doppler_hz: f64,
    paths: Vec<Path>,
    rician_k_db: Option<f64>,
    burst: (usize, usize),
}

fn path(delay_us: f64, power_db: f64, frequency_hz: f64) -> Path {
    Path {
        delay_us,
        power_db,
        frequency_hz,
    }
}

fn cases() -> Vec<Case> {
    let single = || vec![path(0.0, 0.0, 0.0)];
    // Two simulcast base stations: 40 µs apart, 3 dB weaker, beating at 15 Hz.
    let simulcast = || vec![path(0.0, 0.0, 0.0), path(40.0, -3.0, 15.0)];
    let mut list = Vec::new();
    for traffic in [Traffic::Voice, Traffic::Data] {
        let t = |v: &'static str, d: &'static str| if traffic == Traffic::Voice { v } else { d };
        list.extend([
            Case {
                name: t("voice static 6.5 dB", "data static 7.5 dB"),
                traffic,
                es_n0_db: if traffic == Traffic::Voice { 6.5 } else { 7.5 },
                doppler_hz: 0.0,
                paths: single(),
                rician_k_db: None,
                burst: (0, 0),
            },
            Case {
                name: t("voice Rayleigh 10 Hz 14 dB", "data Rayleigh 10 Hz 12 dB"),
                traffic,
                es_n0_db: if traffic == Traffic::Voice {
                    14.0
                } else {
                    12.0
                },
                doppler_hz: 10.0,
                paths: single(),
                rician_k_db: None,
                burst: (0, 0),
            },
            Case {
                name: t("voice Rayleigh 33 Hz 16 dB", "data Rayleigh 33 Hz 14 dB"),
                traffic,
                es_n0_db: if traffic == Traffic::Voice {
                    16.0
                } else {
                    14.0
                },
                doppler_hz: 33.0,
                paths: single(),
                rician_k_db: None,
                burst: (0, 0),
            },
            Case {
                name: t("voice Rician K6 33 Hz 9 dB", "data Rician K6 33 Hz 8 dB"),
                traffic,
                es_n0_db: if traffic == Traffic::Voice { 9.0 } else { 8.0 },
                doppler_hz: 33.0,
                paths: single(),
                rician_k_db: Some(6.0),
                burst: (0, 0),
            },
            Case {
                name: t("voice simulcast 33 Hz 18 dB", "data simulcast 33 Hz 16 dB"),
                traffic,
                es_n0_db: if traffic == Traffic::Voice {
                    18.0
                } else {
                    16.0
                },
                doppler_hz: 33.0,
                paths: simulcast(),
                rician_k_db: None,
                burst: (0, 0),
            },
            Case {
                name: t(
                    "voice bursts 2/8 static 8 dB",
                    "data bursts 2/8 static 9 dB",
                ),
                traffic,
                es_n0_db: if traffic == Traffic::Voice { 8.0 } else { 9.0 },
                doppler_hz: 0.0,
                paths: single(),
                rician_k_db: None,
                burst: (2, 8),
            },
            Case {
                name: t(
                    "voice bursts 1/9 static 9 dB",
                    "data bursts 1/9 static 9 dB",
                ),
                traffic,
                es_n0_db: 9.0,
                doppler_hz: 0.0,
                paths: single(),
                rician_k_db: None,
                burst: (1, 9),
            },
        ]);
    }
    list
}

fn main() {
    if let Err(e) = bench() {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

fn bench() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut seeds = 4u64;
    let mut frames = 250usize;
    let mut only = None;
    let mut json = false;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--json" {
            json = true;
            i += 1;
            continue;
        }
        let value = args.get(i + 1).ok_or("option needs a value")?;
        match args[i].as_str() {
            "--seeds" => seeds = value.parse()?,
            "--frames" => frames = value.parse()?,
            "--only" => only = Some(value.clone()),
            "--features" => receiver_core::tuning::set(jr_demod_eval::parse_features(value)?),
            "--dsp" => jr_conventional_dsp::tuning::parse(value)?,
            other => return Err(format!("unknown option {other}").into()),
        }
        i += 2;
    }
    let mut rows = Vec::new();
    println!("| condition | accepted | undetected | alternate | combined | CPU (% real time) |");
    println!("|---|---:|---:|---:|---:|---:|");
    let mut cpu = 0.0;
    let mut signal = 0.0;
    for case in cases() {
        if only
            .as_ref()
            .is_some_and(|o| !case.name.contains(o.as_str()))
        {
            continue;
        }
        let mut total = Report::default();
        for seed in 1..=seeds {
            let condition = Condition {
                traffic: case.traffic,
                es_n0_db: Some(case.es_n0_db),
                doppler_hz: case.doppler_hz,
                paths: case.paths.clone(),
                rician_k_db: case.rician_k_db,
                burst_on: case.burst.0,
                burst_off: case.burst.1,
                frames,
                seed,
                pipeline: true,
                ..Condition::default()
            };
            total.add(&run(&condition)?);
        }
        let (accepted, undetected) = match case.traffic {
            Traffic::Voice => (total.voice_acceptance(), total.voice_false),
            Traffic::Data => (total.data_acceptance(), total.data_false),
        };
        cpu += total.cpu_seconds;
        signal += total.signal_seconds;
        println!(
            "| {} | {:.2} % | {} | {} | {} | {:.1} |",
            case.name,
            100.0 * accepted,
            undetected,
            total.alternate_accepted,
            total.combined_accepted,
            100.0 * total.cpu_seconds / total.signal_seconds.max(1e-9)
        );
        rows.push(serde_json::json!({
            "condition": case.name,
            "accepted": accepted,
            "undetected": undetected,
            "cpu": total.cpu_seconds / total.signal_seconds.max(1e-9),
        }));
    }
    println!(
        "| **all** | | | | | **{:.1}** |",
        100.0 * cpu / signal.max(1e-9)
    );
    if json {
        eprintln!("{}", serde_json::to_string(&rows)?);
    }
    Ok(())
}
