//! Immutable learned source, spectrum, gain and temporal parameter predictors.
use std::collections::BTreeMap;
use std::sync::OnceLock;

struct Array {
    shape: Vec<usize>,
    data: Vec<f32>,
}
fn arrays(bytes: &[u8]) -> BTreeMap<String, Array> {
    fn integer(bytes: &[u8], at: &mut usize) -> usize {
        let v = u32::from_le_bytes(bytes[*at..*at + 4].try_into().unwrap()) as usize;
        *at += 4;
        v
    }
    assert_eq!(&bytes[..4], b"HML1");
    let mut at = 4;
    let count = integer(bytes, &mut at);
    let mut out = BTreeMap::new();
    for _ in 0..count {
        let length = bytes[at] as usize;
        at += 1;
        let name = std::str::from_utf8(&bytes[at..at + length])
            .unwrap()
            .to_owned();
        at += length;
        let rank = bytes[at] as usize;
        at += 1;
        let shape: Vec<usize> = (0..rank).map(|_| integer(bytes, &mut at)).collect();
        let length = shape.iter().product::<usize>();
        let data: Vec<f32> = (0..length)
            .map(|_| {
                let v = f32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
                at += 4;
                assert!(v.is_finite());
                v
            })
            .collect();
        assert!(out.insert(name, Array { shape, data }).is_none());
    }
    assert_eq!(at, bytes.len());
    out
}
struct Layer {
    input: usize,
    output: usize,
    weight: Vec<f32>,
    bias: Vec<f32>,
}
impl Layer {
    fn apply(&self, input: &[f32], activation: bool) -> Vec<f32> {
        assert_eq!(input.len(), self.input);
        (0..self.output)
            .map(|o| {
                let mut sum = 0.0_f32;
                for i in 0..self.input {
                    sum += input[i] * self.weight[o * self.input + i];
                }
                let sum = sum + self.bias[o];
                if activation {
                    sum.tanh()
                } else {
                    sum
                }
            })
            .collect()
    }
}
pub struct Network {
    mean: Vec<f32>,
    scale: Vec<f32>,
    layers: Vec<Layer>,
    output_scale: Vec<f32>,
}
impl Network {
    fn new(bytes: &[u8]) -> Self {
        let mut a = arrays(bytes);
        let mean = a.remove("mean").unwrap().data;
        let scale = a.remove("scale").unwrap().data;
        assert_eq!(mean.len(), scale.len());
        assert!(scale.iter().all(|&x| x > 0.0));
        let mut layers = Vec::new();
        while let Some(w) = a.remove(&format!("w{}", layers.len())) {
            assert_eq!(w.shape.len(), 2);
            let bias = a.remove(&format!("b{}", layers.len())).unwrap().data;
            assert_eq!(bias.len(), w.shape[0]);
            layers.push(Layer {
                input: w.shape[1],
                output: w.shape[0],
                weight: w.data,
                bias,
            });
        }
        let output_scale = a
            .remove("output_scale")
            .map_or_else(|| vec![1.0; layers.last().unwrap().output], |v| v.data);
        assert!(a.is_empty());
        assert_eq!(layers[0].input, mean.len());
        Self {
            mean,
            scale,
            layers,
            output_scale,
        }
    }
    fn normalize(&self, x: &[f32]) -> Vec<f32> {
        assert_eq!(x.len(), self.mean.len());
        x.iter()
            .enumerate()
            .map(|(i, &v)| ((v - self.mean[i]) / self.scale[i]).clamp(-6.0, 6.0))
            .collect()
    }
    pub fn forward(&self, x: &[f32]) -> Vec<f32> {
        let mut value = self.normalize(x);
        for (i, layer) in self.layers.iter().enumerate() {
            value = layer.apply(&value, i + 1 < self.layers.len());
        }
        for (v, s) in value.iter_mut().zip(&self.output_scale) {
            *v *= s;
        }
        value
    }
    pub fn temporal_embedding(&self, x: &[f32; 707]) -> [f32; 128] {
        self.layers[0]
            .apply(&self.normalize(x), true)
            .try_into()
            .unwrap()
    }
    pub fn temporal_finish(&self, context: &[[f32; 128]; 7]) -> [f32; 34] {
        let mut value: Vec<f32> = context.iter().flatten().copied().collect();
        for (i, layer) in self.layers.iter().enumerate().skip(1) {
            value = layer.apply(&value, i + 1 < self.layers.len());
        }
        value.try_into().unwrap()
    }
}
pub struct Gain {
    mean: Vec<f32>,
    scale: Vec<f32>,
    weight: Vec<f32>,
}
impl Gain {
    fn new(bytes: &[u8]) -> Self {
        let mut a = arrays(bytes);
        let mean = a.remove("mean").unwrap().data;
        let scale = a.remove("scale").unwrap().data;
        let weight = a.remove("weight").unwrap();
        assert_eq!(mean.len(), 137);
        assert_eq!(scale.len(), 137);
        assert_eq!(weight.shape, [138, 2]);
        assert!(scale.iter().all(|&x| x > 0.0));
        assert!(a.is_empty());
        Self {
            mean,
            scale,
            weight: weight.data,
        }
    }
    pub fn predict(&self, x: &[f32; 137]) -> [f32; 2] {
        let z: [f32; 137] =
            std::array::from_fn(|i| ((x[i] - self.mean[i]) / self.scale[i]).clamp(-6.0, 6.0));
        std::array::from_fn(|o| {
            let mut sum = 0.0_f32;
            for i in 0..137 {
                sum += z[i] * self.weight[2 * (i + 1) + o];
            }
            sum + self.weight[o]
        })
    }
}
pub struct Models {
    pub source: Network,
    pub spectrum: Network,
    pub gain: Gain,
    pub temporal: Network,
}
pub fn models() -> &'static Models {
    static VALUE: OnceLock<Models> = OnceLock::new();
    VALUE.get_or_init(|| Models {
        source: Network::new(include_bytes!("models/source.bin")),
        spectrum: Network::new(include_bytes!("models/spectrum.bin")),
        gain: Gain::new(include_bytes!("models/gain.bin")),
        temporal: Network::new(include_bytes!("models/temporal.bin")),
    })
}
