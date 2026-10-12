//! Fixed-point primitives.
//!
//! The codec's arithmetic is that of a 16-bit fixed-point machine with
//! fractional multiplies, sign-extended memory operands and a wide accumulator
//! that wraps.  That gives three properties the code below reproduces:
//!
//! * multiplies are *fractional*: every product is shifted left by one so that
//!   Q15 x Q15 lands in Q31 rather than Q30 ([`mul`]);
//! * accumulators are 40 bit and **wrap** rather than saturate — saturation only
//!   happens where the code explicitly asks for it ([`sat`]);
//! * memory operands are sign extended into the accumulator.
//!
//! Everything is expressed as a plain `i64` holding the 40-bit accumulator
//! value; [`acc`] folds a result back into that range.

/// Fold a value into the 40-bit accumulator range (two's complement wrap).
#[inline]
pub fn acc(v: i64) -> i64 {
    (v << 24) >> 24
}

/// Saturate a 40-bit accumulator to 32 bits.
#[inline]
pub fn sat(v: i64) -> i64 {
    v.clamp(i32::MIN as i64, i32::MAX as i64)
}

/// High half of an accumulator.
#[inline]
pub fn hi(v: i64) -> i16 {
    (v >> 16) as i16
}

/// Low half of an accumulator.
#[inline]
pub fn low(v: i64) -> i16 {
    v as i16
}

/// Fractional multiply: Q15 x Q15 -> Q31, i.e. `x * y << 1`.
#[inline]
pub fn mul(x: i16, y: i16) -> i64 {
    (x as i64) * (y as i64) * 2
}

/// Fractional multiply of an unsigned 16-bit operand by a signed one.
#[inline]
pub fn mul_us(x: i16, y: i16) -> i64 {
    (x as u16 as i64) * (y as i64) * 2
}

/// 32x16 fractional multiply, built from the operand's two halves.
/// `x` is a 32-bit accumulator value, `y` a Q15 coefficient.
#[inline]
pub fn mul32x16(x: i64, y: i16) -> i64 {
    let partial = acc(mul_us(low(x), y)) >> 16;
    acc(partial + mul(hi(x), y))
}

/// Keep only the 32 bits a store/load pair round-trips through memory.
#[inline]
pub fn trunc32(v: i64) -> i64 {
    v as i32 as i64
}

/// Redundant-sign-bit count of a 40-bit accumulator: the left shift
/// that would normalise it against bit 31.  Zero for a zero accumulator.
pub fn exp(v: i64) -> i32 {
    let v = acc(v);
    if v == 0 {
        return 0;
    }
    let mag = if v < 0 { !v } else { v };
    let top = 63 - mag.leading_zeros() as i32; // index of the highest set bit
    30 - top
}

/// Arithmetic shift of an accumulator by a signed amount.
#[inline]
pub fn shift(v: i64, s: i32) -> i64 {
    if s >= 0 {
        acc(v << s)
    } else {
        acc(v >> (-s))
    }
}

/// One step of a conditional subtract-and-shift restoring division.
#[inline]
fn restoring_step(num: i64, den: i16) -> i64 {
    let alu = num - ((den as i64) << 15);
    acc(if alu >= 0 { (alu << 1) + 1 } else { num << 1 })
}

/// Restoring division, one bit at a time, of `a / den`.
/// The quotient accumulates in the low bits of the returned accumulator.
pub fn restoring_divide(mut num: i64, den: i16, steps: u32) -> i64 {
    for _ in 0..steps {
        num = restoring_step(num, den);
    }
    num
}

/// Reciprocal-style divide used throughout: normalises `value`, then divides
/// 16383 by the normalised magnitude.  Returns `(quotient, shift)` where
/// `shift` is the exponent that has to be applied to the quotient afterwards.
pub fn normalised_reciprocal(value: i64) -> (i64, i32) {
    let e = exp(value);
    let shift_out = e + 1;
    let normalised = shift(value, e);
    let magnitude = sat(if normalised < 0 {
        -normalised
    } else {
        normalised
    }) >> 16;
    let quotient = restoring_divide((16383i64) << 16, magnitude as i16, 15);
    // The quotient sits in the low 16 bits; move it to the high half.
    let quotient = acc(((quotient as u64 as i64) & 0xffff) << 16);
    if hi(normalised) < 0 {
        (acc(-quotient), shift_out)
    } else {
        (quotient, shift_out)
    }
}
