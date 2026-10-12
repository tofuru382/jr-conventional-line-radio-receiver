//! Typed views of the embedded predictor coefficients.
use crate::models::binary;
const DATA: &[u8; 310866] = include_bytes!("../models/predictor.bin");
use super::{Mlp, Tensor};
pub static BASELINE_BIAS_DATA: [f32; 17] = binary::floats::<17>(DATA, 0);

pub static BASELINE_WEIGHT_DATA: [f32; 4760] = binary::floats::<4760>(DATA, 68);

pub static CALIBRATION_BIAS_DATA: [f32; 32] = binary::floats::<32>(DATA, 19108);

pub static CALIBRATION_MEAN_DATA: [f32; 67] = binary::floats::<67>(DATA, 19236);

pub static CALIBRATION_SCALE_DATA: [f32; 67] = binary::floats::<67>(DATA, 19504);

pub static CALIBRATION_WEIGHT_DATA: [f32; 2144] = binary::floats::<2144>(DATA, 19772);

pub static CONTEXT_LAYER_0_BIAS_DATA: [f32; 64] = binary::floats::<64>(DATA, 28348);

pub static CONTEXT_LAYER_0_WEIGHT_Q: [i8; 64448] = binary::signed_bytes::<64448>(DATA, 28604);

pub static CONTEXT_LAYER_1_BIAS_DATA: [f32; 32] = binary::floats::<32>(DATA, 93052);

pub static CONTEXT_LAYER_1_WEIGHT_DATA: [f32; 2048] = binary::floats::<2048>(DATA, 93180);

pub static CONTEXT_LAYER_2_BIAS_DATA: [f32; 17] = binary::floats::<17>(DATA, 101372);

pub static CONTEXT_LAYER_2_WEIGHT_DATA: [f32; 544] = binary::floats::<544>(DATA, 101440);

pub static CONTEXT_X_MEAN_DATA: [f32; 1007] = binary::floats::<1007>(DATA, 103616);

pub static CONTEXT_X_SCALE_DATA: [f32; 1007] = binary::floats::<1007>(DATA, 107644);

pub static CONTEXT_Y_MEAN_DATA: [f32; 17] = binary::floats::<17>(DATA, 111672);

pub static CONTEXT_Y_SCALE_DATA: [f32; 17] = binary::floats::<17>(DATA, 111740);

pub static LEVELS_LAYER_0_BIAS_DATA: [f32; 32] = binary::floats::<32>(DATA, 111808);

pub static LEVELS_LAYER_0_WEIGHT_DATA: [f32; 1632] = binary::floats::<1632>(DATA, 111936);

pub static LEVELS_LAYER_1_BIAS_DATA: [f32; 32] = binary::floats::<32>(DATA, 118464);

pub static LEVELS_LAYER_1_WEIGHT_DATA: [f32; 1024] = binary::floats::<1024>(DATA, 118592);

pub static LEVELS_LAYER_2_BIAS_DATA: [f32; 17] = binary::floats::<17>(DATA, 122688);

pub static LEVELS_LAYER_2_WEIGHT_DATA: [f32; 544] = binary::floats::<544>(DATA, 122756);

pub static LEVELS_X_MEAN_DATA: [f32; 51] = binary::floats::<51>(DATA, 124932);

pub static LEVELS_X_SCALE_DATA: [f32; 51] = binary::floats::<51>(DATA, 125136);

pub static LEVELS_Y_MEAN_DATA: [f32; 17] = binary::floats::<17>(DATA, 125340);

pub static LEVELS_Y_SCALE_DATA: [f32; 17] = binary::floats::<17>(DATA, 125408);

pub static PROFILE_WEIGHT_DATA: [f32; 11067] = binary::floats::<11067>(DATA, 125476);

pub static RECURRENT_GATE_BIAS_DATA: [f32; 192] = binary::floats::<192>(DATA, 169744);

pub static RECURRENT_INPUT_WEIGHT_Q: [i8; 3824] = binary::signed_bytes::<3824>(DATA, 170512);

pub static RECURRENT_INPUT_WEIGHT_ROWS: [u8; 3824] = binary::bytes::<3824>(DATA, 174336);

pub static RECURRENT_INPUT_WEIGHT_OFFSETS: [u16; 193] = binary::words::<193>(DATA, 178160);

pub static RECURRENT_OUTPUT_BIAS_DATA: [f32; 70] = binary::floats::<70>(DATA, 178546);

pub static RECURRENT_OUTPUT_WEIGHT_Q: [i8; 4480] = binary::signed_bytes::<4480>(DATA, 178826);

pub static RECURRENT_RECURRENT_WEIGHT_Q: [i8; 2249] = binary::signed_bytes::<2249>(DATA, 183306);

pub static RECURRENT_RECURRENT_WEIGHT_ROWS: [u8; 2249] = binary::bytes::<2249>(DATA, 185555);

pub static RECURRENT_RECURRENT_WEIGHT_OFFSETS: [u16; 193] = binary::words::<193>(DATA, 187804);

pub static RECURRENT_X_MEAN_DATA: [f32; 81] = binary::floats::<81>(DATA, 188190);

pub static RECURRENT_X_SCALE_DATA: [f32; 81] = binary::floats::<81>(DATA, 188514);

pub static RECURRENT_Y_MEAN_DATA: [f32; 70] = binary::floats::<70>(DATA, 188838);

pub static RECURRENT_Y_SCALE_DATA: [f32; 70] = binary::floats::<70>(DATA, 189118);

pub static REFINEMENT_PITCH_BIAS_DATA: [f32; 1] = binary::floats::<1>(DATA, 189398);

pub static REFINEMENT_PITCH_MEAN_DATA: [f32; 37] = binary::floats::<37>(DATA, 189402);

pub static REFINEMENT_PITCH_SCALE_DATA: [f32; 37] = binary::floats::<37>(DATA, 189550);

pub static REFINEMENT_PITCH_WEIGHT_DATA: [f32; 37] = binary::floats::<37>(DATA, 189698);

pub static REFINEMENT_SPECTRAL_BIAS_DATA: [f32; 16] = binary::floats::<16>(DATA, 189846);

pub static REFINEMENT_SPECTRAL_MEAN_DATA: [f32; 36] = binary::floats::<36>(DATA, 189910);

pub static REFINEMENT_SPECTRAL_SCALE_DATA: [f32; 36] = binary::floats::<36>(DATA, 190054);

pub static REFINEMENT_SPECTRAL_TABLE_DATA: [f32; 8192] = binary::floats::<8192>(DATA, 190198);

pub static REFINEMENT_SPECTRAL_WEIGHT_DATA: [f32; 576] = binary::floats::<576>(DATA, 222966);

pub static SPECTRUM_LAYER_0_BIAS_DATA: [f32; 64] = binary::floats::<64>(DATA, 225270);

pub static SPECTRUM_LAYER_0_WEIGHT_Q: [i8; 64448] = binary::signed_bytes::<64448>(DATA, 225526);

pub static SPECTRUM_LAYER_1_BIAS_DATA: [f32; 32] = binary::floats::<32>(DATA, 289974);

pub static SPECTRUM_LAYER_1_WEIGHT_DATA: [f32; 2048] = binary::floats::<2048>(DATA, 290102);

pub static SPECTRUM_LAYER_2_BIAS_DATA: [f32; 17] = binary::floats::<17>(DATA, 298294);

pub static SPECTRUM_LAYER_2_WEIGHT_DATA: [f32; 544] = binary::floats::<544>(DATA, 298362);

pub static SPECTRUM_X_MEAN_DATA: [f32; 1007] = binary::floats::<1007>(DATA, 300538);

pub static SPECTRUM_X_SCALE_DATA: [f32; 1007] = binary::floats::<1007>(DATA, 304566);

pub static SPECTRUM_Y_MEAN_DATA: [f32; 17] = binary::floats::<17>(DATA, 308594);

pub static SPECTRUM_Y_SCALE_DATA: [f32; 17] = binary::floats::<17>(DATA, 308662);

pub static VOICING_BIAS_DATA: [f32; 2] = binary::floats::<2>(DATA, 308730);

pub static VOICING_WEIGHT_DATA: [f32; 168] = binary::floats::<168>(DATA, 308738);

pub static VOICING_X_MEAN_DATA: [f32; 84] = binary::floats::<84>(DATA, 309410);

pub static VOICING_X_SCALE_DATA: [f32; 84] = binary::floats::<84>(DATA, 309746);

pub static CONTEXT_BLEND: [f32; 17] = binary::floats::<17>(DATA, 310082);

pub static LEVEL_BLEND: [f32; 17] = binary::floats::<17>(DATA, 310150);

pub static SPECTRAL_BLEND: [f32; 17] = binary::floats::<17>(DATA, 310218);

pub static RECURRENT_BLEND: [f32; 70] = binary::floats::<70>(DATA, 310286);

pub const BASELINE_BIAS: Tensor = Tensor::float(&BASELINE_BIAS_DATA, 17, 1);
pub const BASELINE_WEIGHT: Tensor = Tensor::float(&BASELINE_WEIGHT_DATA, 280, 17);
pub const CALIBRATION_BIAS: Tensor = Tensor::float(&CALIBRATION_BIAS_DATA, 32, 1);
pub const CALIBRATION_MEAN: Tensor = Tensor::float(&CALIBRATION_MEAN_DATA, 67, 1);
pub const CALIBRATION_SCALE: Tensor = Tensor::float(&CALIBRATION_SCALE_DATA, 67, 1);
pub const CALIBRATION_WEIGHT: Tensor = Tensor::float(&CALIBRATION_WEIGHT_DATA, 67, 32);

pub const CONTEXT_LAYER_0_BIAS: Tensor = Tensor::float(&CONTEXT_LAYER_0_BIAS_DATA, 64, 1);
pub const CONTEXT_LAYER_0_WEIGHT: Tensor = Tensor::quantized(
    &CONTEXT_LAYER_0_WEIGHT_Q,
    1007,
    64,
    binary::floats::<1>(DATA, 310850)[0],
    &[],
    &[],
);
pub const CONTEXT_LAYER_1_BIAS: Tensor = Tensor::float(&CONTEXT_LAYER_1_BIAS_DATA, 32, 1);
pub const CONTEXT_LAYER_1_WEIGHT: Tensor = Tensor::float(&CONTEXT_LAYER_1_WEIGHT_DATA, 64, 32);
pub const CONTEXT_LAYER_2_BIAS: Tensor = Tensor::float(&CONTEXT_LAYER_2_BIAS_DATA, 17, 1);
pub const CONTEXT_LAYER_2_WEIGHT: Tensor = Tensor::float(&CONTEXT_LAYER_2_WEIGHT_DATA, 32, 17);

pub const CONTEXT_X_MEAN: Tensor = Tensor::float(&CONTEXT_X_MEAN_DATA, 1007, 1);
pub const CONTEXT_X_SCALE: Tensor = Tensor::float(&CONTEXT_X_SCALE_DATA, 1007, 1);
pub const CONTEXT_Y_MEAN: Tensor = Tensor::float(&CONTEXT_Y_MEAN_DATA, 17, 1);
pub const CONTEXT_Y_SCALE: Tensor = Tensor::float(&CONTEXT_Y_SCALE_DATA, 17, 1);

pub const LEVELS_LAYER_0_BIAS: Tensor = Tensor::float(&LEVELS_LAYER_0_BIAS_DATA, 32, 1);
pub const LEVELS_LAYER_0_WEIGHT: Tensor = Tensor::float(&LEVELS_LAYER_0_WEIGHT_DATA, 51, 32);
pub const LEVELS_LAYER_1_BIAS: Tensor = Tensor::float(&LEVELS_LAYER_1_BIAS_DATA, 32, 1);
pub const LEVELS_LAYER_1_WEIGHT: Tensor = Tensor::float(&LEVELS_LAYER_1_WEIGHT_DATA, 32, 32);
pub const LEVELS_LAYER_2_BIAS: Tensor = Tensor::float(&LEVELS_LAYER_2_BIAS_DATA, 17, 1);
pub const LEVELS_LAYER_2_WEIGHT: Tensor = Tensor::float(&LEVELS_LAYER_2_WEIGHT_DATA, 32, 17);

pub const LEVELS_X_MEAN: Tensor = Tensor::float(&LEVELS_X_MEAN_DATA, 51, 1);
pub const LEVELS_X_SCALE: Tensor = Tensor::float(&LEVELS_X_SCALE_DATA, 51, 1);
pub const LEVELS_Y_MEAN: Tensor = Tensor::float(&LEVELS_Y_MEAN_DATA, 17, 1);
pub const LEVELS_Y_SCALE: Tensor = Tensor::float(&LEVELS_Y_SCALE_DATA, 17, 1);
pub const PROFILE_WEIGHT: Tensor = Tensor::float(&PROFILE_WEIGHT_DATA, 651, 17);

pub const RECURRENT_GATE_BIAS: Tensor = Tensor::float(&RECURRENT_GATE_BIAS_DATA, 192, 1);

pub const RECURRENT_INPUT_WEIGHT: Tensor = Tensor::quantized(
    &RECURRENT_INPUT_WEIGHT_Q,
    81,
    192,
    binary::floats::<1>(DATA, 310854)[0],
    &RECURRENT_INPUT_WEIGHT_ROWS,
    &RECURRENT_INPUT_WEIGHT_OFFSETS,
);
pub const RECURRENT_OUTPUT_BIAS: Tensor = Tensor::float(&RECURRENT_OUTPUT_BIAS_DATA, 70, 1);
pub const RECURRENT_OUTPUT_WEIGHT: Tensor = Tensor::quantized(
    &RECURRENT_OUTPUT_WEIGHT_Q,
    64,
    70,
    binary::floats::<1>(DATA, 310858)[0],
    &[],
    &[],
);
pub const RECURRENT_RECURRENT_WEIGHT: Tensor = Tensor::quantized(
    &RECURRENT_RECURRENT_WEIGHT_Q,
    64,
    192,
    binary::floats::<1>(DATA, 310858)[0],
    &RECURRENT_RECURRENT_WEIGHT_ROWS,
    &RECURRENT_RECURRENT_WEIGHT_OFFSETS,
);
pub const RECURRENT_X_MEAN: Tensor = Tensor::float(&RECURRENT_X_MEAN_DATA, 81, 1);
pub const RECURRENT_X_SCALE: Tensor = Tensor::float(&RECURRENT_X_SCALE_DATA, 81, 1);
pub const RECURRENT_Y_MEAN: Tensor = Tensor::float(&RECURRENT_Y_MEAN_DATA, 70, 1);
pub const RECURRENT_Y_SCALE: Tensor = Tensor::float(&RECURRENT_Y_SCALE_DATA, 70, 1);
pub const REFINEMENT_PITCH_BIAS: Tensor = Tensor::float(&REFINEMENT_PITCH_BIAS_DATA, 1, 1);
pub const REFINEMENT_PITCH_MEAN: Tensor = Tensor::float(&REFINEMENT_PITCH_MEAN_DATA, 37, 1);
pub const REFINEMENT_PITCH_SCALE: Tensor = Tensor::float(&REFINEMENT_PITCH_SCALE_DATA, 37, 1);
pub const REFINEMENT_PITCH_WEIGHT: Tensor = Tensor::float(&REFINEMENT_PITCH_WEIGHT_DATA, 37, 1);
pub const REFINEMENT_SPECTRAL_BIAS: Tensor = Tensor::float(&REFINEMENT_SPECTRAL_BIAS_DATA, 16, 1);
pub const REFINEMENT_SPECTRAL_MEAN: Tensor = Tensor::float(&REFINEMENT_SPECTRAL_MEAN_DATA, 36, 1);
pub const REFINEMENT_SPECTRAL_SCALE: Tensor = Tensor::float(&REFINEMENT_SPECTRAL_SCALE_DATA, 36, 1);
pub const REFINEMENT_SPECTRAL_TABLE: Tensor =
    Tensor::float(&REFINEMENT_SPECTRAL_TABLE_DATA, 512, 16);
pub const REFINEMENT_SPECTRAL_WEIGHT: Tensor =
    Tensor::float(&REFINEMENT_SPECTRAL_WEIGHT_DATA, 36, 16);

pub const SPECTRUM_LAYER_0_BIAS: Tensor = Tensor::float(&SPECTRUM_LAYER_0_BIAS_DATA, 64, 1);
pub const SPECTRUM_LAYER_0_WEIGHT: Tensor = Tensor::quantized(
    &SPECTRUM_LAYER_0_WEIGHT_Q,
    1007,
    64,
    binary::floats::<1>(DATA, 310862)[0],
    &[],
    &[],
);
pub const SPECTRUM_LAYER_1_BIAS: Tensor = Tensor::float(&SPECTRUM_LAYER_1_BIAS_DATA, 32, 1);
pub const SPECTRUM_LAYER_1_WEIGHT: Tensor = Tensor::float(&SPECTRUM_LAYER_1_WEIGHT_DATA, 64, 32);
pub const SPECTRUM_LAYER_2_BIAS: Tensor = Tensor::float(&SPECTRUM_LAYER_2_BIAS_DATA, 17, 1);
pub const SPECTRUM_LAYER_2_WEIGHT: Tensor = Tensor::float(&SPECTRUM_LAYER_2_WEIGHT_DATA, 32, 17);

pub const SPECTRUM_X_MEAN: Tensor = Tensor::float(&SPECTRUM_X_MEAN_DATA, 1007, 1);
pub const SPECTRUM_X_SCALE: Tensor = Tensor::float(&SPECTRUM_X_SCALE_DATA, 1007, 1);
pub const SPECTRUM_Y_MEAN: Tensor = Tensor::float(&SPECTRUM_Y_MEAN_DATA, 17, 1);
pub const SPECTRUM_Y_SCALE: Tensor = Tensor::float(&SPECTRUM_Y_SCALE_DATA, 17, 1);
pub const VOICING_BIAS: Tensor = Tensor::float(&VOICING_BIAS_DATA, 2, 1);
pub const VOICING_WEIGHT: Tensor = Tensor::float(&VOICING_WEIGHT_DATA, 84, 2);
pub const VOICING_X_MEAN: Tensor = Tensor::float(&VOICING_X_MEAN_DATA, 84, 1);
pub const VOICING_X_SCALE: Tensor = Tensor::float(&VOICING_X_SCALE_DATA, 84, 1);
pub const CONTEXT: Mlp = Mlp {
    x_mean: CONTEXT_X_MEAN,
    x_scale: CONTEXT_X_SCALE,
    y_mean: CONTEXT_Y_MEAN,
    y_scale: CONTEXT_Y_SCALE,
    weight: [
        CONTEXT_LAYER_0_WEIGHT,
        CONTEXT_LAYER_1_WEIGHT,
        CONTEXT_LAYER_2_WEIGHT,
    ],
    bias: [
        CONTEXT_LAYER_0_BIAS,
        CONTEXT_LAYER_1_BIAS,
        CONTEXT_LAYER_2_BIAS,
    ],
};
pub const LEVELS: Mlp = Mlp {
    x_mean: LEVELS_X_MEAN,
    x_scale: LEVELS_X_SCALE,
    y_mean: LEVELS_Y_MEAN,
    y_scale: LEVELS_Y_SCALE,
    weight: [
        LEVELS_LAYER_0_WEIGHT,
        LEVELS_LAYER_1_WEIGHT,
        LEVELS_LAYER_2_WEIGHT,
    ],
    bias: [
        LEVELS_LAYER_0_BIAS,
        LEVELS_LAYER_1_BIAS,
        LEVELS_LAYER_2_BIAS,
    ],
};
pub const SPECTRUM: Mlp = Mlp {
    x_mean: SPECTRUM_X_MEAN,
    x_scale: SPECTRUM_X_SCALE,
    y_mean: SPECTRUM_Y_MEAN,
    y_scale: SPECTRUM_Y_SCALE,
    weight: [
        SPECTRUM_LAYER_0_WEIGHT,
        SPECTRUM_LAYER_1_WEIGHT,
        SPECTRUM_LAYER_2_WEIGHT,
    ],
    bias: [
        SPECTRUM_LAYER_0_BIAS,
        SPECTRUM_LAYER_1_BIAS,
        SPECTRUM_LAYER_2_BIAS,
    ],
};

pub const PITCH_BLEND: f32 = binary::floats::<1>(DATA, 310566)[0];
pub const PROFILE_GROUPS: [[usize; 8]; 7] = binary::index_rows::<7, 8>(DATA, 310570);
pub const PROFILE_LENGTHS: [usize; 7] = binary::indices::<7>(DATA, 310794);
pub const PROFILE_OFFSETS: [usize; 7] = binary::indices::<7>(DATA, 310822);
