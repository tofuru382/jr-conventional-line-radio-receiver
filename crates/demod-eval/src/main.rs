//! Demodulator evaluation tool. Not part of the receiver or its releases.
use std::error::Error;
type Result<T> = std::result::Result<T, Box<dyn Error>>;
const HELP: &str = "jr-demod-eval: demodulator evaluation with synthetic signals (development tool)

  jr-demod-eval [--es-n0 DB,…|none] [--doppler HZ] [--paths US:DB[:HZ],…] [--rician-k DB]
                [--offset HZ] [--rate S/s] [--frames N] [--seeds N]
                [--receiver dsp|differential] [--traffic voice|data] [--hard] [--single]

Synthesizes frames, a fading/simulcast channel and noise, receives them with the
production DSP and decoders, and reports symbol errors and frame acceptance per
Es/N0: a summary on standard error, JSON on standard output.
Defaults: none, 0 Hz, 0:0, 48000 S/s, 200 frames, 1 seed, dsp, voice, soft decisions,
both detectors. --receiver differential is a genie-timed reference detector.
";
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{HELP}");
        return;
    }
    if let Err(e) = evaluate(&args) {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
fn evaluate(args: &[String]) -> Result<()> {
    use jr_demod_eval::{run, Condition, Path as ChannelPath, Receiver, Report, Traffic};
    let mut base = Condition::default();
    let mut levels = vec![None];
    let mut seeds = 1u64;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--hard" {
            base.soft = false;
            i += 1;
            continue;
        }
        if args[i] == "--single" {
            base.alternate = false;
            i += 1;
            continue;
        }
        let value = args
            .get(i + 1)
            .ok_or_else(|| format!("{} needs a value", args[i]))?;
        match args[i].as_str() {
            "--es-n0" => {
                levels = if value == "none" {
                    vec![None]
                } else {
                    value
                        .split(',')
                        .map(|v| v.trim().parse().map(Some))
                        .collect::<std::result::Result<_, _>>()?
                }
            }
            "--doppler" => base.doppler_hz = value.parse()?,
            "--paths" => {
                base.paths = value
                    .split(',')
                    .map(|p| {
                        let mut fields = p.split(':');
                        let (Some(delay), Some(power)) = (fields.next(), fields.next()) else {
                            return Err("paths use DELAY_US:POWER_DB[:OFFSET_HZ]".into());
                        };
                        Ok(ChannelPath {
                            delay_us: delay.trim().parse()?,
                            power_db: power.trim().parse()?,
                            frequency_hz: fields
                                .next()
                                .map(|f| f.trim().parse())
                                .transpose()?
                                .unwrap_or(0.0),
                        })
                    })
                    .collect::<Result<_>>()?
            }
            "--rician-k" => base.rician_k_db = Some(value.parse()?),
            "--offset" => base.frequency_offset_hz = value.parse()?,
            "--rate" => base.sample_rate = value.parse()?,
            "--frames" => base.frames = value.parse()?,
            "--seeds" => seeds = value.parse()?,
            "--traffic" => {
                base.traffic = match value.as_str() {
                    "voice" => Traffic::Voice,
                    "data" => Traffic::Data,
                    _ => return Err("--traffic must be voice or data".into()),
                }
            }
            "--receiver" => {
                base.receiver = match value.as_str() {
                    "dsp" => Receiver::Dsp,
                    "differential" => Receiver::Differential,
                    _ => return Err("--receiver must be dsp or differential".into()),
                }
            }
            other => return Err(format!("Unknown option {other}\n\n{HELP}").into()),
        }
        i += 2;
    }
    if !(1..=1000).contains(&seeds) {
        return Err("--seeds must be 1–1000".into());
    }
    let mut results = Vec::new();
    for es_n0_db in levels {
        let mut total = Report::default();
        for seed in 1..=seeds {
            let condition = Condition {
                es_n0_db,
                seed,
                ..base.clone()
            };
            total.add(&run(&condition)?);
        }
        eprintln!(
            "Es/N0 {:>8}  SER {:.2e}  BER {:.2e}  {} {:6.2} %  undetected {}  raw-bit BER {:.2e}  unaligned {}  slips {}  alternate {}",
            es_n0_db.map_or("none".into(), |v| format!("{v:.1} dB")),
            total.symbol_error_rate(),
            total.bit_error_rate(),
            if base.traffic == Traffic::Data { "data " } else { "voice" },
            100.0 * if base.traffic == Traffic::Data {
                total.data_acceptance()
            } else {
                total.voice_acceptance()
            },
            total.voice_false + total.data_false,
            total.unprotected_bit_errors as f64 / total.unprotected_bits.max(1) as f64,
            total.symbols_unaligned,
            total.clock_slips,
            total.alternate_accepted,
        );
        if total
            .fade_bins
            .iter()
            .any(|b| b.symbols > 0 && b.below_db != Some(0.0))
        {
            let bins: Vec<String> = total
                .fade_bins
                .iter()
                .filter(|b| b.symbols > 0)
                .map(|b| {
                    format!(
                        "{}: {:.1e} ({:.1} %)",
                        b.below_db.map_or("≥0 dB".into(), |e| format!("<{e:.0} dB")),
                        b.errors as f64 / b.symbols as f64,
                        100.0 * b.symbols as f64 / total.symbols_compared.max(1) as f64
                    )
                })
                .collect();
            eprintln!("    SER by fade depth  {}", bins.join("  "));
        }
        let mut row = serde_json::to_value(&total)?;
        row["es_n0_db"] = serde_json::json!(es_n0_db);
        row["eb_n0_db"] = serde_json::json!(es_n0_db.map(|v| v - 10f64.log10() * 2.0));
        row["symbol_error_rate"] = serde_json::json!(finite(total.symbol_error_rate()));
        row["bit_error_rate"] = serde_json::json!(finite(total.bit_error_rate()));
        row["voice_acceptance"] = serde_json::json!(finite(total.voice_acceptance()));
        row["data_acceptance"] = serde_json::json!(finite(total.data_acceptance()));
        results.push(row);
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "condition": Condition { es_n0_db: None, seed: 0, ..base },
            "seeds": seeds,
            "results": results,
        }))?
    );
    Ok(())
}
fn finite(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}
