//! Fractional-delay interpolation filter for pitch prediction.

/// 1/3-resolution pitch interpolation filter, 31 taps.
pub static PITCH_INTERP: [i16; 31] = [
    29443, 25207, 14701, 3143, -4402, -5850, -2783, 1211, 3130, 2259, 0, -1652, -1666, -464, 756,
    1099, 550, -245, -634, -451, 0, 308, 296, 78, -120, -165, -79, 34, 91, 70, 0,
];
