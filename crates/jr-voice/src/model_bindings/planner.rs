// Fixed planner weights and measurement constants.
use crate::models::binary;
const DATA: &[u8; 63542] = include_bytes!("../models/planner.bin");
pub mod input_parameters {
    use super::{binary, DATA};
    pub static LSF_MEAN: [f32; 10] = binary::floats::<10>(DATA, 0);
    pub static SPECTRAL: [f32; 1920] = binary::floats::<1920>(DATA, 40);
    pub static MA: [f32; 80] = binary::floats::<80>(DATA, 7720);
    pub static GAIN_LOG: [f32; 128] = binary::floats::<128>(DATA, 8040);
    pub static GAIN_ADAPTIVE: [f32; 128] = binary::floats::<128>(DATA, 8552);
    pub static CLASS_BASE: [u16; 6] = binary::words::<6>(DATA, 9064);
    pub static INDEX_LIMIT: [u16; 1] = binary::words::<1>(DATA, 9076);
    pub static PULSES: [i16; 5] = binary::signed_words::<5>(DATA, 9078);
    pub static RADIX: [i16; 15] = binary::signed_words::<15>(DATA, 9088);
    pub static RADIX_INVERSE: [i16; 15] = binary::signed_words::<15>(DATA, 9118);
    pub static POSITIONS: [i16; 600] = binary::signed_words::<600>(DATA, 9148);
    pub static SHAPE_SELECT: [i16; 15] = binary::signed_words::<15>(DATA, 10348);
    pub static SHAPES: [i16; 240] = binary::signed_words::<240>(DATA, 10378);
    pub static ALT_A: [i16; 640] = binary::signed_words::<640>(DATA, 10858);
    pub static ALT_B: [i16; 640] = binary::signed_words::<640>(DATA, 12138);
    pub const INITIAL_GAIN: f32 = binary::floats::<1>(DATA, 13418)[0];
}
pub mod learned_parameters {
    use super::{binary, DATA};
    pub static Q__CURRENT_CDF_BASIS: [i8; 2240] = binary::signed_bytes::<2240>(DATA, 13422);
    pub static Q__CURRENT_CDF_WEIGHT: [i8; 3520] = binary::signed_bytes::<3520>(DATA, 15662);
    pub static Q__CURRENT_WEIGHT: [i8; 10120] = binary::signed_bytes::<10120>(DATA, 19182);
    pub static Q__NET_W1: [i8; 3520] = binary::signed_bytes::<3520>(DATA, 29302);
    pub static Q__NET_W2: [i8; 128] = binary::signed_bytes::<128>(DATA, 32822);
    pub static Q__NET_W3: [i8; 8] = binary::signed_bytes::<8>(DATA, 32950);
    pub static Q__PREVIOUS_CDF_BASIS: [i8; 2240] = binary::signed_bytes::<2240>(DATA, 32958);
    pub static Q__PREVIOUS_CDF_WEIGHT: [i8; 3520] = binary::signed_bytes::<3520>(DATA, 35198);
    pub static Q__PREVIOUS_WEIGHT: [i8; 440] = binary::signed_bytes::<440>(DATA, 38718);
    pub static Q__TRAILING_CDF_BASIS: [i8; 2240] = binary::signed_bytes::<2240>(DATA, 39158);
    pub static Q__TRAILING_CDF_WEIGHT: [i8; 3520] = binary::signed_bytes::<3520>(DATA, 41398);
    pub static Q__TRAILING_WEIGHT: [i8; 440] = binary::signed_bytes::<440>(DATA, 44918);
    pub static S__CURRENT_CDF_BASIS: [f32; 140] = binary::floats::<140>(DATA, 45358);
    pub static S__CURRENT_CDF_WEIGHT: [f32; 16] = binary::floats::<16>(DATA, 45918);
    pub static S__CURRENT_WEIGHT: [f32; 46] = binary::floats::<46>(DATA, 45982);
    pub static S__NET_W1: [f32; 16] = binary::floats::<16>(DATA, 46166);
    pub static S__NET_W2: [f32; 8] = binary::floats::<8>(DATA, 46230);
    pub static S__NET_W3: [f32; 1] = binary::floats::<1>(DATA, 46262);
    pub static S__PREVIOUS_CDF_BASIS: [f32; 140] = binary::floats::<140>(DATA, 46266);
    pub static S__PREVIOUS_CDF_WEIGHT: [f32; 16] = binary::floats::<16>(DATA, 46826);
    pub static S__PREVIOUS_WEIGHT: [f32; 2] = binary::floats::<2>(DATA, 46890);
    pub static S__TRAILING_CDF_BASIS: [f32; 140] = binary::floats::<140>(DATA, 46898);
    pub static S__TRAILING_CDF_WEIGHT: [f32; 16] = binary::floats::<16>(DATA, 47458);
    pub static S__TRAILING_WEIGHT: [f32; 2] = binary::floats::<2>(DATA, 47522);
    pub static V__CURRENT_BIAS: [f32; 46] = binary::floats::<46>(DATA, 47530);
    pub static V__CURRENT_CDF_BIAS: [f32; 16] = binary::floats::<16>(DATA, 47714);
    pub static V__CURRENT_CDF_MEAN: [f32; 140] = binary::floats::<140>(DATA, 47778);
    pub static V__CURRENT_VOICE_RATE: [f32; 1] = binary::floats::<1>(DATA, 48338);
    pub static V__MEAN: [f32; 220] = binary::floats::<220>(DATA, 48342);
    pub static V__NET_B1: [f32; 16] = binary::floats::<16>(DATA, 49222);
    pub static V__NET_B2: [f32; 8] = binary::floats::<8>(DATA, 49286);
    pub static V__NET_B3: [f32; 1] = binary::floats::<1>(DATA, 49318);
    pub static V__PREVIOUS_BIAS: [f32; 2] = binary::floats::<2>(DATA, 49322);
    pub static V__PREVIOUS_CDF_BIAS: [f32; 16] = binary::floats::<16>(DATA, 49330);
    pub static V__PREVIOUS_CDF_MEAN: [f32; 140] = binary::floats::<140>(DATA, 49394);
    pub static V__PREVIOUS_VOICE_RATE: [f32; 1] = binary::floats::<1>(DATA, 49954);
    pub static V__SCALE: [f32; 220] = binary::floats::<220>(DATA, 49958);
    pub static V__TRAILING_BIAS: [f32; 2] = binary::floats::<2>(DATA, 50838);
    pub static V__TRAILING_CDF_BIAS: [f32; 16] = binary::floats::<16>(DATA, 50846);
    pub static V__TRAILING_CDF_MEAN: [f32; 140] = binary::floats::<140>(DATA, 50910);
    pub static V__TRAILING_VOICE_RATE: [f32; 1] = binary::floats::<1>(DATA, 51470);
    pub static LOG_LAGS: [f32; 141] = binary::floats::<141>(DATA, 51474);
    pub static EDGES: [f32; 140] = binary::floats::<140>(DATA, 52038);
}
pub mod measurement_parameters {
    use super::{binary, DATA};
    pub static F32_TWIDDLE_REAL: [f32; 1024] = binary::floats::<1024>(DATA, 52598);
    pub static F32_TWIDDLE_IMAG: [f32; 1024] = binary::floats::<1024>(DATA, 56694);
    pub static F32_HANN: [f32; 160] = binary::floats::<160>(DATA, 60790);
    pub static F32_BAND_WEIGHTS: [f32; 486] = binary::floats::<486>(DATA, 61430);
    pub static F32_BAND_START: [usize; 20] = binary::indices::<20>(DATA, 63374);
    pub static F32_BAND_OFFSET: [usize; 21] = binary::indices::<21>(DATA, 63454);
    pub static F32_HANN_ENERGY: [f32; 1] = binary::floats::<1>(DATA, 63538);
}
