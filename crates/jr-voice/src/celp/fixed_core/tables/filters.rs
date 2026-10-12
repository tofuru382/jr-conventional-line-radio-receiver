//! Fixed output reconstruction coefficients.

/// Coefficients of the output filter's feed-forward part.
pub static OUT_FIR: [i16; 4] = [7472, -22381, 22381, -7472];

/// Coefficients of its feedback part.
pub static OUT_IIR: [i16; 3] = [-23033, 21666, -6815];
