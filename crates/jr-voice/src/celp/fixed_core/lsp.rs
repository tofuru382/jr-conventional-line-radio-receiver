//! Line spectral conversions and interpolation.
//!
//! Representations used here:
//!
//! * **LSF** — line spectral *frequencies* in radians, Q13 (`pi` = 25736).
//! * **LSP** — line spectral *pairs*, i.e. `cos(lsf)`, Q15.
//! * **LPC** — direct-form predictor `a[0..=10]`, Q12 with `a[0] = 4096`.

use crate::celp::fixed_core::fixed::{acc, hi, mul, mul32x16, sat, shift, trunc32};

/// Predictor order.
pub use crate::celp::fixed_core::LPC_ORDER as ORDER;

/// `cos()` of an LSF vector: LSF (Q13 radians) -> LSP (Q15), by table lookup
/// with linear interpolation.
pub fn lsf_to_lsp(lsf: &[i16; ORDER]) -> [i16; ORDER] {
    let mut lsp = [0i16; ORDER];
    for i in 0..ORDER {
        // 20861 in Q15, doubled by the fractional multiply, is 4/pi: it
        // scales a Q13 radian value to a 6.8 fixed-point table index.
        let scaled = mul(lsf[i], 20861);
        let index = hi(shift(scaled, -8).min(63 << 16)) as usize;
        let frac = (hi(scaled) & 0xff) as i64;
        let interp = acc(((crate::celp::fixed_core::tables::COS[index] as i64) << 16)
            + (frac << 3) * (crate::celp::fixed_core::tables::COS_SLOPE[index] as i64) * 2);
        lsp[i] = hi(interp);
    }
    lsp
}

/// Symmetric/antisymmetric polynomial of five LSPs.
///
/// Produces `f[0..=5]` in Q24; `lsp` is read with a stride of two.
fn lsp_polynomial(lsp: &[i16], first: usize) -> [i64; 6] {
    let mut f = [0i64; 6];
    f[0] = 1 << 24;
    f[1] = trunc32(-((lsp[first] as i64) << 10));
    for i in 2..=5 {
        let l = -lsp[first + 2 * (i - 1)];
        f[i] = trunc32(acc(2 * f[i - 2] + (mul32x16(f[i - 1], l) << 1)));
        for j in (2..i).rev() {
            f[j] = trunc32(acc(f[j] + (mul32x16(f[j - 1], l) << 1) + f[j - 2]));
        }
        f[1] = trunc32(acc(f[1] + ((l as i64) << 10)));
    }
    f
}

/// LSP -> direct-form LPC.
///
/// `A(z) = (F1(z)(1 + z^-1) + F2(z)(1 - z^-1)) / 2` with `F1`/`F2` built from
/// the even and odd LSPs.
pub fn lsp_to_lpc(lsp: &[i16; ORDER]) -> [i16; ORDER + 1] {
    let mut f1 = lsp_polynomial(lsp, 0);
    let mut f2 = lsp_polynomial(lsp, 1);
    for i in (1..=5).rev() {
        f1[i] = trunc32(acc(f1[i] + f1[i - 1]));
        f2[i] = trunc32(acc(f2[i] - f2[i - 1]));
    }

    let mut a = [0i16; ORDER + 1];
    a[0] = 4096;
    for i in 1..=5 {
        a[i] = hi(acc(shift(shift(acc(f1[i] + f2[i]), -1), 4) + (1 << 15)));
        a[ORDER + 1 - i] = hi(acc(shift(shift(acc(f1[i] - f2[i]), -1), 4) + (1 << 15)));
    }
    a
}

/// Build the two LSP sets a half-frame needs and convert both to LPC.
///
/// The first subframe of the pair uses the midpoint between the previous and
/// current LSP sets, the second uses the current set unchanged.
pub fn interpolate_pair(
    prev: &[i16; ORDER],
    cur: &[i16; ORDER],
) -> ([i16; ORDER + 1], [i16; ORDER + 1]) {
    let mid = midpoint(prev, cur);
    (lsp_to_lpc(&mid), lsp_to_lpc(cur))
}

/// Midpoint of two line spectral pairs.
///
/// The subframe filters are built from the two half-frame vectors and the point
/// halfway between them; this is that midpoint.
pub fn midpoint(first: &[i16; ORDER], second: &[i16; ORDER]) -> [i16; ORDER] {
    let mut out = [0i16; ORDER];
    for i in 0..ORDER {
        // The sum, halved as it is stored.
        out[i] = hi(acc(((first[i] as i64) + (second[i] as i64)) << 15));
    }
    out
}

/// LPC -> reflection coefficients, by running the Levinson recursion backwards.
///
/// Each step peels the last coefficient off as a reflection coefficient and
/// divides the rest by `1 - k^2`, updating the array from both ends at once.
pub fn reflection_coefficients(lpc: &[i16; ORDER + 1]) -> [i16; ORDER] {
    let mut k = [0i16; ORDER];
    k.copy_from_slice(&lpc[1..]);

    for step in 0..ORDER - 1 {
        let order = ORDER - step;
        let top = order - 1;
        let kv = k[top];

        // 1 - k^2 in Q30, then its reciprocal.
        let residual = acc((16384i64 << 16) - shift(acc((kv as i64) * (kv as i64) * 2), 5));
        let (quotient, exponent) = crate::celp::fixed_core::fixed::normalised_reciprocal(residual);
        let scale = (exponent << 27) >> 27; // a signed 5-bit shift field
        let inverse = hi(shift(quotient, -1));

        let pairs = ((order - 2) >> 1) + 1;
        for p in 0..pairs {
            let (i, j) = (p, top - 1 - p);
            let low = acc(((k[i] as i64) << 16) - shift(acc(mul(k[j], kv)), 3));
            let updated_low = hi(shift(mul32x16(sat(low), inverse), scale));
            let high = acc(((k[j] as i64) << 16) - shift(acc(mul(kv, k[i])), 3));
            let updated_high = hi(shift(mul32x16(sat(high), inverse), scale));
            k[i] = updated_low;
            k[j] = updated_high;
        }
    }
    k
}
