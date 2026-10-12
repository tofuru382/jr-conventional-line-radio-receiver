//! Typed views of the embedded predictor coefficients.
use crate::models::binary;
const DATA: &[u8; 18020] = include_bytes!("../models/correction.bin");
use super::Int8Matrix;
pub static GAIN_MEAN: [f32; 12] = binary::floats::<12>(DATA, 0);

pub static GAIN_SCALE: [f32; 12] = binary::floats::<12>(DATA, 48);

pub static GAIN_WEIGHT: [f32; 141] = binary::floats::<141>(DATA, 96);

pub static LINEAR_MEAN: [f32; 36] = binary::floats::<36>(DATA, 660);

pub static LINEAR_SCALE: [f32; 36] = binary::floats::<36>(DATA, 804);

pub static LINEAR_COEFFICIENT_Q: [i8; 8784] = binary::signed_bytes::<8784>(DATA, 948);

pub static LINEAR_COEFFICIENT_SCALE: [f32; 16] = binary::floats::<16>(DATA, 9732);

pub static MAIN_TABLE: [i16; 1280] = binary::signed_words::<1280>(DATA, 9796);

pub static STUDENT_X_MEAN: [f32; 145] = binary::floats::<145>(DATA, 12356);

pub static STUDENT_X_SCALE: [f32; 145] = binary::floats::<145>(DATA, 12936);

pub static STUDENT_Y_MEAN: [f32; 16] = binary::floats::<16>(DATA, 13516);

pub static STUDENT_Y_SCALE: [f32; 16] = binary::floats::<16>(DATA, 13580);

pub static STUDENT_LAYER_0_WEIGHT_Q: [i8; 3480] = binary::signed_bytes::<3480>(DATA, 13644);

pub static STUDENT_LAYER_0_WEIGHT_SCALE: [f32; 24] = binary::floats::<24>(DATA, 17124);

pub static STUDENT_LAYER_0_BIAS: [f32; 24] = binary::floats::<24>(DATA, 17220);

pub static STUDENT_LAYER_1_WEIGHT_Q: [i8; 288] = binary::signed_bytes::<288>(DATA, 17316);

pub static STUDENT_LAYER_1_WEIGHT_SCALE: [f32; 12] = binary::floats::<12>(DATA, 17604);

pub static STUDENT_LAYER_1_BIAS: [f32; 12] = binary::floats::<12>(DATA, 17652);

pub static STUDENT_LAYER_2_WEIGHT_Q: [i8; 192] = binary::signed_bytes::<192>(DATA, 17700);

pub static STUDENT_LAYER_2_WEIGHT_SCALE: [f32; 16] = binary::floats::<16>(DATA, 17892);

pub static STUDENT_LAYER_2_BIAS: [f32; 16] = binary::floats::<16>(DATA, 17956);

pub const LINEAR_COEFFICIENT: Int8Matrix = Int8Matrix {
    data: &LINEAR_COEFFICIENT_Q,
    scale: &LINEAR_COEFFICIENT_SCALE,
    columns: 16,
};
pub const STUDENT_LAYER_0_WEIGHT: Int8Matrix = Int8Matrix {
    data: &STUDENT_LAYER_0_WEIGHT_Q,
    scale: &STUDENT_LAYER_0_WEIGHT_SCALE,
    columns: 24,
};
pub const STUDENT_LAYER_1_WEIGHT: Int8Matrix = Int8Matrix {
    data: &STUDENT_LAYER_1_WEIGHT_Q,
    scale: &STUDENT_LAYER_1_WEIGHT_SCALE,
    columns: 12,
};
pub const STUDENT_LAYER_2_WEIGHT: Int8Matrix = Int8Matrix {
    data: &STUDENT_LAYER_2_WEIGHT_Q,
    scale: &STUDENT_LAYER_2_WEIGHT_SCALE,
    columns: 16,
};
