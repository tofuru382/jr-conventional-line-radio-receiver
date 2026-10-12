#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HalfParams {
    pub gain_low: [Option<u8>; 2],
    pub code: [u16; 2],
    pub gain_high: [u8; 2],
    pub mode: u8,
    pub stage1: u8,
    pub stage2_low: u8,
    pub stage2_high: u8,
    pub absolute_pitch: u8,
    pub relative_pitch: u8,
}

impl HalfParams {
    pub fn gain_index(&self, sub: usize) -> usize {
        ((self.gain_high[sub] & 7) as usize) * 16
            + (self.gain_low[sub].or(self.gain_low[0]).unwrap_or(0) & 15) as usize
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameParams {
    pub halves: [HalfParams; 2],
}

fn read(values: impl IntoIterator<Item = u8>) -> u16 {
    values.into_iter().fold(0, |v, b| 2 * v + u16::from(b & 1))
}

pub fn unpack(input: &[u8; 158]) -> FrameParams {
    let halves = std::array::from_fn(|h| {
        let q = |k| input[84 + 39 * h + k];
        let a = |k| input[20 * h + k];
        let b = |k| input[40 + 20 * h + k];
        let code = |k: usize| {
            read([
                q(if k == 0 { 18 } else { 30 }),
                q(if k == 0 { 19 } else { 31 }),
                b(7 + k),
                b(5 + k),
                a(6 + k),
                a(4 + k),
                b(6 + k),
                b(4 + k),
                a(11 + k),
                a(9 + k),
                b(11 + k),
                b(9 + k),
                a(10 + k),
                a(8 + k),
                b(10 + k),
                b(8 + k),
            ])
        };
        HalfParams {
            gain_low: [
                Some(read((23..27).map(q)) as u8),
                if h == 0 {
                    Some(read((35..39).map(q)) as u8)
                } else {
                    None
                },
            ],
            code: [code(0), code(8)],
            gain_high: [read((20..23).map(q)) as u8, read((32..35).map(q)) as u8],
            mode: q(0) & 1,
            stage1: read((1..8).map(q)) as u8,
            stage2_low: read([q(8), q(9), q(10), q(11), a(3), a(1)]) as u8,
            stage2_high: read([b(3), b(1), a(2), a(0), b(2), b(0)]) as u8,
            absolute_pitch: read([q(12), q(13), q(14), q(15), q(16), q(17), a(7), a(5)]) as u8,
            relative_pitch: read([q(27), q(28), q(29), a(15), a(13)]) as u8,
        }
    });
    FrameParams { halves }
}
