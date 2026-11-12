// Indexed elimination follows the reference matrix coordinates directly.
#![allow(clippy::needless_range_loop)]
use std::sync::OnceLock;

pub fn descramble(envelope: &[u8; 48]) -> [u8; 42] {
    let mut pn = [1u8; 336];
    let mut word = [0; 42];
    for i in 0..336 {
        if i >= 9 {
            pn[i] = pn[i - 9] ^ pn[i - 5];
        }
        let pos = 128 * (i / 112) + 16 + i % 112;
        word[i / 8] |= (((envelope[pos / 8] >> (7 - pos % 8)) & 1) ^ pn[i]) << (i % 8);
    }
    word
}
/// Inverse of [`descramble`]: place a 42-byte word and the three sync markers
/// into a 384-bit radio envelope. Only tests and development tools encode.
#[cfg(any(test, feature = "synthesis"))]
pub fn scramble(word: &[u8; 42]) -> [u8; 48] {
    let mut pn = [1u8; 336];
    let mut envelope = [0; 48];
    for (m, marker) in [0x3d22u16, 0xe867, 0xe867].iter().enumerate() {
        envelope[16 * m..16 * m + 2].copy_from_slice(&marker.to_be_bytes());
    }
    for i in 0..336 {
        if i >= 9 {
            pn[i] = pn[i - 9] ^ pn[i - 5];
        }
        let pos = 128 * (i / 112) + 16 + i % 112;
        envelope[pos / 8] |= (((word[i / 8] >> (i % 8)) & 1) ^ pn[i]) << (7 - pos % 8);
    }
    envelope
}
pub fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = 0xffffu16;
    for &b in bytes {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = (crc << 1) ^ if crc & 0x8000 != 0 { 0x1021 } else { 0 };
        }
    }
    crc
}
pub struct Galois {
    pub powers: [u8; 510],
    logs: [usize; 256],
}
impl Galois {
    pub fn get() -> &'static Self {
        static GF: OnceLock<Galois> = OnceLock::new();
        GF.get_or_init(|| {
            let mut g = Galois {
                powers: [0; 510],
                logs: [0; 256],
            };
            let mut v = 1usize;
            for i in 0..255 {
                g.powers[i] = v as u8;
                g.powers[i + 255] = v as u8;
                g.logs[v] = i;
                v <<= 1;
                if v & 256 != 0 {
                    v ^= 0x11d;
                }
            }
            g
        })
    }
    pub fn mul(&self, a: u8, b: u8) -> u8 {
        if a == 0 || b == 0 {
            0
        } else {
            self.powers[self.logs[a as usize] + self.logs[b as usize]]
        }
    }
    pub fn div(&self, a: u8, b: u8) -> u8 {
        assert_ne!(b, 0);
        if a == 0 {
            0
        } else {
            self.powers[(self.logs[a as usize] + 255 - self.logs[b as usize]) % 255]
        }
    }
    fn syndromes(&self, word: &[u8], roots: usize) -> Vec<u8> {
        (0..roots)
            .map(|r| word.iter().fold(0, |v, b| self.mul(v, self.powers[r]) ^ b))
            .collect()
    }
    /// Berlekamp–Massey, Chien search and a bounded GF linear solve. All roots verified.
    pub fn correct(&self, word: &mut [u8], roots: usize) -> Option<usize> {
        let syn = self.syndromes(word, roots);
        if syn.iter().all(|s| *s == 0) {
            return Some(0);
        }
        let mut loc = vec![0; roots + 1];
        let mut prev = loc.clone();
        loc[0] = 1;
        prev[0] = 1;
        let (mut degree, mut gap, mut last) = (0, 1, 1);
        for step in 0..roots {
            let mut d = syn[step];
            for i in 1..=degree {
                d ^= self.mul(loc[i], syn[step - i]);
            }
            if d == 0 {
                gap += 1;
                continue;
            }
            let saved = loc.clone();
            let scale = self.div(d, last);
            for i in 0..roots + 1 - gap {
                loc[i + gap] ^= self.mul(scale, prev[i]);
            }
            if 2 * degree <= step {
                degree = step + 1 - degree;
                prev = saved;
                last = d;
                gap = 1;
            } else {
                gap += 1;
            }
        }
        if degree == 0 || degree > roots / 2 {
            return None;
        }
        let n = word.len();
        let positions: Vec<_> = (0..n)
            .filter(|&p| {
                let inverse = self.powers[(p + 255 - (n - 1)) % 255];
                (0..=degree)
                    .rev()
                    .fold(0, |v, i| self.mul(v, inverse) ^ loc[i])
                    == 0
            })
            .collect();
        if positions.len() != degree {
            return None;
        }
        let mut mat = vec![vec![0; degree + 1]; degree];
        for row in 0..degree {
            for col in 0..degree {
                mat[row][col] = self.powers[(row * (n - 1 - positions[col])) % 255];
            }
            mat[row][degree] = syn[row];
        }
        for col in 0..degree {
            let pivot = (col..degree).find(|&r| mat[r][col] != 0)?;
            mat.swap(pivot, col);
            let scale = mat[col][col];
            for i in col..=degree {
                mat[col][i] = self.div(mat[col][i], scale);
            }
            for row in 0..degree {
                if row == col {
                    continue;
                }
                let factor = mat[row][col];
                for i in col..=degree {
                    mat[row][i] ^= self.mul(factor, mat[col][i]);
                }
            }
        }
        for i in 0..degree {
            word[positions[i]] ^= mat[i][degree];
        }
        self.syndromes(word, roots)
            .iter()
            .all(|s| *s == 0)
            .then_some(degree)
    }
}

impl Galois {
    #[cfg(any(test, feature = "synthesis"))]
    /// Systematic Reed–Solomon parity for the decoder's convention: roots
    /// alpha^0..alpha^(roots-1), first byte highest degree. Fills the last
    /// `roots` bytes of `word` so that every syndrome is zero.
    pub fn encode(&self, word: &mut [u8], roots: usize) {
        let mut generator = vec![1u8];
        for r in 0..roots {
            let root = self.powers[r];
            let mut next = vec![0u8; generator.len() + 1];
            for (i, &g) in generator.iter().enumerate() {
                next[i] ^= g;
                next[i + 1] ^= self.mul(g, root);
            }
            generator = next;
        }
        let k = word.len() - roots;
        let mut remainder = vec![0u8; roots];
        for &m in &word[..k] {
            let feedback = m ^ remainder[0];
            remainder.rotate_left(1);
            remainder[roots - 1] = 0;
            for (r, &g) in remainder.iter_mut().zip(&generator[1..]) {
                *r ^= self.mul(feedback, g);
            }
        }
        word[k..].copy_from_slice(&remainder);
    }

    /// Errors-and-erasures decoding. `erasures` are byte positions believed
    /// unreliable; with f erasures and e errors it succeeds when 2e + f <=
    /// roots. All syndromes are verified after correction. Returns the number
    /// of bytes changed.
    pub fn correct_with_erasures(
        &self,
        word: &mut [u8],
        roots: usize,
        erasures: &[usize],
    ) -> Option<usize> {
        let n = word.len();
        if erasures.len() > roots || erasures.iter().any(|&p| p >= n) {
            return None;
        }
        let syn = self.syndromes(word, roots);
        if syn.iter().all(|s| *s == 0) {
            return Some(0);
        }
        let locator = |p: usize| self.powers[(n - 1 - p) % 255];
        // Erasure locator Gamma(x) = prod (1 + X_j x), lowest degree first.
        let mut gamma = vec![1u8];
        for &p in erasures {
            let x = locator(p);
            let mut next = vec![0u8; gamma.len() + 1];
            for (i, &g) in gamma.iter().enumerate() {
                next[i] ^= g;
                next[i + 1] ^= self.mul(g, x);
            }
            gamma = next;
        }
        // Forney syndromes: T(x) = S(x) Gamma(x) mod x^roots, keep T_f..T_(roots-1).
        let mut forney = vec![0u8; roots];
        for (i, &s) in syn.iter().enumerate() {
            for (j, &g) in gamma.iter().enumerate() {
                if i + j < roots {
                    forney[i + j] ^= self.mul(s, g);
                }
            }
        }
        let sequence = &forney[erasures.len()..];
        let (lambda, degree) = self.berlekamp_massey(sequence)?;
        let mut positions: Vec<usize> = (0..n)
            .filter(|&p| {
                let inverse = self.powers[(p + 255 - (n - 1)) % 255];
                (0..=degree)
                    .rev()
                    .fold(0, |v, i| self.mul(v, inverse) ^ lambda[i])
                    == 0
            })
            .collect();
        if positions.len() != degree {
            return None;
        }
        for &p in erasures {
            if !positions.contains(&p) {
                positions.push(p);
            }
        }
        let count = positions.len();
        if count > roots {
            return None;
        }
        // Magnitudes from the first `count` syndromes (Vandermonde system).
        let mut mat = vec![vec![0; count + 1]; count];
        for row in 0..count {
            for col in 0..count {
                mat[row][col] = self.powers[(row * (n - 1 - positions[col])) % 255];
            }
            mat[row][count] = syn[row];
        }
        for col in 0..count {
            let pivot = (col..count).find(|&r| mat[r][col] != 0)?;
            mat.swap(pivot, col);
            let scale = mat[col][col];
            for i in col..=count {
                mat[col][i] = self.div(mat[col][i], scale);
            }
            for row in 0..count {
                if row != col {
                    let factor = mat[row][col];
                    for i in col..=count {
                        mat[row][i] ^= self.mul(factor, mat[col][i]);
                    }
                }
            }
        }
        let mut changed = 0;
        for i in 0..count {
            word[positions[i]] ^= mat[i][count];
            changed += usize::from(mat[i][count] != 0);
        }
        self.syndromes(word, roots)
            .iter()
            .all(|s| *s == 0)
            .then_some(changed)
    }

    /// Berlekamp–Massey over `sequence`; the locator is lowest degree first.
    fn berlekamp_massey(&self, sequence: &[u8]) -> Option<(Vec<u8>, usize)> {
        let len = sequence.len();
        let mut loc = vec![0u8; len + 1];
        let mut prev = loc.clone();
        loc[0] = 1;
        prev[0] = 1;
        let (mut degree, mut gap, mut last) = (0usize, 1usize, 1u8);
        for step in 0..len {
            let mut d = sequence[step];
            for i in 1..=degree.min(step) {
                d ^= self.mul(loc[i], sequence[step - i]);
            }
            if d == 0 {
                gap += 1;
                continue;
            }
            let saved = loc.clone();
            let scale = self.div(d, last);
            for i in 0..(len + 1).saturating_sub(gap) {
                loc[i + gap] ^= self.mul(scale, prev[i]);
            }
            if 2 * degree <= step {
                degree = step + 1 - degree;
                prev = saved;
                last = d;
                gap = 1;
            } else {
                gap += 1;
            }
        }
        (2 * degree <= len).then_some((loc, degree))
    }
}

#[cfg(test)]
mod erasure_tests {
    use super::*;

    fn rng(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    #[test]
    fn encoder_matches_the_voice_tail_generator_and_zeroes_syndromes() {
        let gf = Galois::get();
        let mut word = [0u8; 14];
        word[..8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        gf.encode(&mut word, 6);
        // Same LFSR as voice_frame::tail with taps [1, 63, 1, 218, 32, 227, 38].
        let mut work = [0u8; 14];
        work[..8].copy_from_slice(&word[..8]);
        for i in 0..8 {
            let c = work[i];
            for (j, g) in [1, 63, 1, 218, 32, 227, 38].iter().enumerate() {
                work[i + j] ^= gf.mul(c, *g);
            }
        }
        assert_eq!(&word[8..], &work[8..]);
        assert!(gf.syndromes(&word, 6).iter().all(|s| *s == 0));
    }

    #[test]
    fn errors_and_erasures_within_capacity_are_corrected() {
        let gf = Galois::get();
        let mut state = 0x1234_5678_9abc_def1u64;
        for trial in 0..2000 {
            let mut word = [0u8; 42];
            for b in &mut word[..30] {
                *b = rng(&mut state) as u8;
            }
            gf.encode(&mut word, 12);
            let original = word;
            let erasures_count = (trial % 13) as usize;
            let errors_count = (12 - erasures_count) / 2;
            let mut positions: Vec<usize> = Vec::new();
            while positions.len() < erasures_count + errors_count {
                let p = (rng(&mut state) % 42) as usize;
                if !positions.contains(&p) {
                    positions.push(p);
                }
            }
            for &p in &positions {
                word[p] ^= (rng(&mut state) as u8) | 1;
            }
            let erasures = &positions[..erasures_count];
            assert!(
                gf.correct_with_erasures(&mut word, 12, erasures).is_some(),
                "trial {trial}"
            );
            assert_eq!(word, original, "trial {trial}");
        }
    }

    #[test]
    fn erasure_decoding_agrees_with_errors_only_decoding() {
        let gf = Galois::get();
        let mut state = 0x0bad_cafe_dead_beefu64;
        for _ in 0..2000 {
            let mut word = [0u8; 42];
            for b in &mut word[..30] {
                *b = rng(&mut state) as u8;
            }
            gf.encode(&mut word, 12);
            for _ in 0..(rng(&mut state) % 9) {
                let p = (rng(&mut state) % 42) as usize;
                word[p] ^= rng(&mut state) as u8;
            }
            let (mut a, mut b) = (word, word);
            let ra = gf.correct(&mut a, 12).is_some();
            let rb = gf.correct_with_erasures(&mut b, 12, &[]).is_some();
            assert_eq!(ra, rb);
            if ra {
                assert_eq!(a, b);
            }
        }
    }
}
