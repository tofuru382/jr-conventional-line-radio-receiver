//! Immutable production coefficients, shared by all decoder instances.
#[path = "model_bindings/correction.rs"]
pub mod correction;
#[path = "model_bindings/predictor.rs"]
pub mod predictor;

#[derive(Clone, Copy)]
pub struct Tensor {
    pub data: &'static [f32],
    pub rows: usize,
    pub cols: usize,
    pub quantized: &'static [i8],
    pub quantum: f32,
    pub sparse_rows: &'static [u8],
    pub sparse_offsets: &'static [u16],
}

impl Tensor {
    pub const fn float(data: &'static [f32], rows: usize, cols: usize) -> Self {
        Self {
            data,
            rows,
            cols,
            quantized: &[],
            quantum: 1.0,
            sparse_rows: &[],
            sparse_offsets: &[],
        }
    }
    pub const fn quantized(
        data: &'static [i8],
        rows: usize,
        cols: usize,
        quantum: f32,
        sparse_rows: &'static [u8],
        sparse_offsets: &'static [u16],
    ) -> Self {
        Self {
            data: &[],
            rows,
            cols,
            quantized: data,
            quantum,
            sparse_rows,
            sparse_offsets,
        }
    }
    pub fn get(self, i: usize) -> f32 {
        if !self.sparse_offsets.is_empty() {
            let (col, row) = (i % self.cols, i / self.cols);
            for p in self.sparse_offsets[col] as usize..self.sparse_offsets[col + 1] as usize {
                if self.sparse_rows[p] as usize == row {
                    return self.quantized[p] as f32 * self.quantum;
                }
            }
            0.0
        } else if !self.quantized.is_empty() {
            self.quantized[i] as f32 * self.quantum
        } else {
            self.data[i]
        }
    }
}

pub struct Mlp {
    pub x_mean: Tensor,
    pub x_scale: Tensor,
    pub y_mean: Tensor,
    pub y_scale: Tensor,
    pub weight: [Tensor; 3],
    pub bias: [Tensor; 3],
}

pub struct Int8Matrix {
    pub data: &'static [i8],
    pub scale: &'static [f32],
    pub columns: usize,
}
impl Int8Matrix {
    pub fn get(&self, i: usize) -> f32 {
        self.data[i] as f32 * self.scale[i % self.columns]
    }
    pub fn dense(&self, input: &[f32], bias: &[f32], output: &mut [f32]) {
        for j in 0..self.columns {
            let mut sum = 0.0;
            for (i, &x) in input.iter().enumerate() {
                sum += x * self.data[i * self.columns + j] as f32;
            }
            output[j] = sum * self.scale[j] + bias[j];
        }
    }
}

#[path = "model_bindings/planner.rs"]
pub mod planner;

#[path = "model_bindings/binary.rs"]
mod binary;
