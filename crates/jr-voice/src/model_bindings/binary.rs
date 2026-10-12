//! Compile-time, alignment-independent readers for embedded coefficient tables.
// Every multi-byte value is little-endian. Indices occupy u32 on disk, including
// on 64-bit hosts. Static initializers use these readers without runtime loading.

macro_rules! reader {
    ($name:ident, $ty:ty, $zero:expr, $width:literal, $raw:ident, $value:expr) => {
        pub const fn $name<const N: usize>(data: &[u8], offset: usize) -> [$ty; N] {
            assert!(offset <= data.len());
            assert!(N <= (data.len() - offset) / $width);
            let mut out = [$zero; N];
            let mut i = 0;
            while i < N {
                let mut $raw = [0; $width];
                let mut b = 0;
                while b < $width {
                    $raw[b] = data[offset + i * $width + b];
                    b += 1;
                }
                out[i] = $value;
                i += 1;
            }
            out
        }
    };
}

reader!(floats, f32, 0.0, 4, raw, f32::from_le_bytes(raw));
reader!(signed_bytes, i8, 0, 1, raw, i8::from_le_bytes(raw));
reader!(bytes, u8, 0, 1, raw, raw[0]);
reader!(signed_words, i16, 0, 2, raw, i16::from_le_bytes(raw));
reader!(words, u16, 0, 2, raw, u16::from_le_bytes(raw));
reader!(indices, usize, 0, 4, raw, u32::from_le_bytes(raw) as usize);

pub const fn index_rows<const R: usize, const C: usize>(
    data: &[u8],
    offset: usize,
) -> [[usize; C]; R] {
    assert!(offset <= data.len());
    assert!(R == 0 || C <= (data.len() - offset) / 4 / R);
    let mut out = [[0; C]; R];
    let mut row = 0;
    while row < R {
        out[row] = indices(data, offset + row * C * 4);
        row += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_float_bits_at_unaligned_offsets() {
        // Negative zero, smallest subnormal, one, and a NaN payload.
        let data = [99, 0, 0, 0, 128, 1, 0, 0, 0, 0, 0, 128, 63, 1, 0, 192, 127];
        assert_eq!(
            floats::<4>(&data, 1).map(f32::to_bits),
            [0x80000000, 1, 0x3f800000, 0x7fc00001]
        );
    }

    #[test]
    fn decodes_signed_values_and_portable_indices() {
        assert_eq!(signed_bytes::<3>(&[128, 255, 127], 0), [-128, -1, 127]);
        assert_eq!(signed_words::<2>(&[0, 128, 255, 127], 0), [-32768, 32767]);
        assert_eq!(words::<2>(&[0, 128, 255, 255], 0), [32768, 65535]);
        let data = [99, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 255, 255, 255, 255];
        assert_eq!(
            index_rows::<2, 2>(&data, 1),
            [[1, 256], [65536, u32::MAX as usize]]
        );
        assert_eq!(bytes::<0>(&data, data.len()), []);
    }

    #[test]
    #[should_panic]
    fn rejects_truncated_arrays() {
        floats::<2>(&[0; 8], 1);
    }
}
