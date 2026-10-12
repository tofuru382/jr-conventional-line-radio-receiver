//! Actual candidate-PCM scoring. Integer centered moments retain exact sums.
use crate::models::planner::measurement_parameters as t;
use crate::plan_target::Target;
fn level(pcm: &[i16]) -> f32 {
    let sum: i64 = pcm.iter().map(|&x| x as i64 * x as i64).sum();
    10.0 * (sum as f32 / 1073741824.0 / pcm.len() as f32)
        .max(1e-12)
        .log10()
}
fn correlation(pcm: &[i16]) -> [f32; 141] {
    let n = pcm.len();
    let mut sums = [0i64; 641];
    let mut squares = [0i64; 641];
    for i in 0..n {
        sums[i + 1] = sums[i] + pcm[i] as i64;
        squares[i + 1] = squares[i] + pcm[i] as i64 * pcm[i] as i64;
    }
    let (size, total) = (n as i64, sums[n]);
    let total_square = total * total;
    let mut out = [0.0; 141];
    for (i, c) in out.iter_mut().enumerate() {
        let lag = i + 20;
        let mut raw = 0i64;
        for j in 0..n - lag {
            raw += pcm[j] as i64 * pcm[j + lag] as i64;
        }
        let (left_sum, right_sum) = (sums[n - lag], total - sums[lag]);
        let extra = (size - lag as i64) * total_square;
        let cov = size * size * raw - size * total * (left_sum + right_sum) + extra;
        let left = size * size * squares[n - lag] - 2 * size * total * left_sum + extra;
        let right =
            size * size * (squares[n] - squares[lag]) - 2 * size * total * right_sum + extra;
        *c = (cov as f32 / (left as f32 * right as f32).max(1e-30).sqrt()).clamp(-1.0, 1.0);
    }
    out
}
fn bands(pcm: &[i16]) -> [f32; 20] {
    let mut real = [0.0; 512];
    let mut imag = [0.0; 512];
    for i in 0..160 {
        real[i] = pcm[i] as f32 / 32768.0 * t::F32_HANN[i];
    }
    let mut j = 0;
    for i in 1..512 {
        let mut bit = 256;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            real.swap(i, j);
            imag.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= 512 {
        let half = len / 2;
        let step = 2048 / len;
        for begin in (0..512).step_by(len) {
            for j in 0..half {
                let a = begin + j;
                let b = a + half;
                let (wr, wi) = (t::F32_TWIDDLE_REAL[j * step], t::F32_TWIDDLE_IMAG[j * step]);
                let br = real[b] * wr - imag[b] * wi;
                let bi = real[b] * wi + imag[b] * wr;
                real[b] = real[a] - br;
                imag[b] = imag[a] - bi;
                real[a] += br;
                imag[a] += bi;
            }
        }
        len *= 2;
    }
    let mut energy = [0.0; 257];
    for i in 0..257 {
        energy[i] = (real[i] * real[i] + imag[i] * imag[i]) / t::F32_HANN_ENERGY[0];
    }
    let mut result = [0.0; 20];
    for band in 0..20 {
        let mut power = 0.0;
        let start = t::F32_BAND_OFFSET[band];
        let end = t::F32_BAND_OFFSET[band + 1];
        for j in start..end {
            power += energy[t::F32_BAND_START[band] + j - start] * t::F32_BAND_WEIGHTS[j];
        }
        result[band] = 10.0 * power.max(1e-12).log10();
    }
    result
}
fn cost(pcm: &[i16], target: &Target, kind: usize) -> f32 {
    let correlation = correlation(pcm);
    let mut strength = -1.0f32;
    for &c in &correlation {
        strength = strength.max(c);
    }
    let mut lag = 0;
    if level(pcm) >= -40.0 && strength >= 0.5 {
        let minimum = 0.35f32.max(0.9 * strength);
        for i in 0..141 {
            if correlation[i] >= minimum
                && (i == 0 || correlation[i] >= correlation[i - 1])
                && (i == 140 || correlation[i] >= correlation[i + 1])
            {
                lag = i + 20;
                break;
            }
        }
    }
    let mean = &target.mean[match kind {
        0 => 0,
        1 => 46,
        _ => 48,
    }..];
    let error = strength - mean[if kind == 0 { 44 } else { 0 }];
    let mut result = error * error / (0.17 * 0.17);
    result += if lag != 0 {
        target.risk[kind][lag - 20]
    } else {
        target.missing[kind]
    };
    if lag == 0 {
        result += target.missing[kind];
    }
    if kind == 0 {
        let mut levels = 0.0;
        let mut band_error = 0.0;
        let mut shape_error = 0.0;
        for i in 0..4 {
            let x = level(&pcm[80 * i..80 * (i + 1)]) - mean[i];
            levels += x * x;
        }
        for half in 0..2 {
            let b = bands(&pcm[160 * half..160 * (half + 1)]);
            let mut center = 0.0;
            for j in 0..20 {
                center += b[j] - mean[4 + half * 20 + j];
            }
            center /= 20.0;
            for j in 0..20 {
                let x = b[j] - mean[4 + half * 20 + j];
                band_error += x * x;
                let y = x - center;
                shape_error += y * y;
            }
        }
        result += 0.25 * (levels / 4.0) / 25.0
            + 0.25 * (band_error / 40.0) / 36.0
            + 0.5 * (shape_error / 40.0) / 16.0;
        result += 2.0 * pcm.iter().filter(|&&x| (x as i32).abs() >= 32767).count() as f32;
    }
    result
}
pub fn frame_cost(
    pcm: &[i16; 320],
    target: &Target,
    past: Option<&[i16; 320]>,
    before: Option<&[i16; 320]>,
) -> f32 {
    let mut value = cost(pcm, target, 0);
    let mut window = [0i16; 640];
    if let (Some(before), Some(past)) = (before, past) {
        window[..160].copy_from_slice(&before[160..]);
        window[160..480].copy_from_slice(past);
        window[480..].copy_from_slice(&pcm[..160]);
        value += 0.5 * cost(&window, target, 1);
    }
    if let Some(past) = past {
        window[..320].copy_from_slice(past);
        window[320..].copy_from_slice(pcm);
        value += 0.5 * cost(&window, target, 2);
    }
    value
}
