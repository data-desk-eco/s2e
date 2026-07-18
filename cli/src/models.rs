//! Native inference for the two published PyTorch checkpoints used by MARS-S2L.
//! Candle reads the upstream `.pt` files directly; there is no conversion step,
//! Python environment, or second set of model artefacts to keep in sync.

use candle_core::{pickle::PthTensors, Device, Tensor};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const MARS_URL: &str = "https://huggingface.co/datasets/UNEP-IMEO/MARS-S2L/resolve/main/trained_models/MARSS2L_20250326/best_epoch?download=true";
pub const MARS_SHA256: &str = "be634fb9e24dc4877f44c1ff9f69972e6f0453e30d70c0dc03677876340ef246";
pub const CLOUD_URL: &str =
    "https://huggingface.co/isp-uv-es/cloudsen12_models/resolve/main/UNetMobV2_V2.pt?download=true";
pub const CLOUD_SHA256: &str = "218fa69aa3c7212d4e690b48af88ac6f3c976fc50d07f275b8fd623909183d7a";

#[derive(Clone, Debug)]
pub struct ModelPaths {
    pub mars: PathBuf,
    pub clouds: PathBuf,
}

impl ModelPaths {
    pub fn in_dir(dir: impl AsRef<Path>) -> Self {
        let dir = dir.as_ref();
        Self {
            mars: dir.join("mars-s2l-20250326.pt"),
            clouds: dir.join("cloudsen12-v2.pt"),
        }
    }

    pub fn default_dir() -> PathBuf {
        std::env::var_os("S2_MODELS")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("XDG_CACHE_HOME")
                    .map(|p| PathBuf::from(p).join("s2-flares/models"))
            })
            .or_else(|| {
                std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".cache/s2-flares/models"))
            })
            .unwrap_or_else(|| PathBuf::from(".models"))
    }

    pub fn ensure(dir: impl AsRef<Path>) -> Result<Self, String> {
        let paths = Self::in_dir(dir);
        fetch(&paths.mars, MARS_URL, MARS_SHA256)?;
        fetch(&paths.clouds, CLOUD_URL, CLOUD_SHA256)?;
        Ok(paths)
    }

    pub fn ensure_clouds(dir: impl AsRef<Path>) -> Result<Self, String> {
        let paths = Self::in_dir(dir);
        fetch(&paths.clouds, CLOUD_URL, CLOUD_SHA256)?;
        Ok(paths)
    }
}

fn sha256(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buf = [0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn fetch(path: &Path, url: &str, expected: &str) -> Result<(), String> {
    if path.exists() && sha256(path)? == expected {
        return Ok(());
    }
    fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("part");
    eprintln!("model: {}", path.display());
    let response = ureq::get(url)
        .call()
        .map_err(|e| format!("download {url}: {e}"))?;
    let mut input = response.into_reader();
    let mut output = File::create(&tmp).map_err(|e| e.to_string())?;
    std::io::copy(&mut input, &mut output).map_err(|e| e.to_string())?;
    output.flush().map_err(|e| e.to_string())?;
    let actual = sha256(&tmp)?;
    if actual != expected {
        return Err(format!(
            "model checksum mismatch: expected {expected}, got {actual}"
        ));
    }
    fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

struct Weights {
    pth: PthTensors,
    device: Device,
    prefix: &'static str,
}

impl Weights {
    fn open(path: &Path, key: Option<&str>, prefix: &'static str) -> Result<Self, String> {
        Ok(Self {
            pth: PthTensors::new(path, key).map_err(|e| format!("read {}: {e}", path.display()))?,
            device: Device::Cpu,
            prefix,
        })
    }

    fn get(&self, name: &str) -> Result<Tensor, String> {
        let full = format!("{}{}", self.prefix, name);
        self.pth
            .get(&full)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("model tensor missing: {full}"))?
            .to_device(&self.device)
            .map_err(|e| e.to_string())
    }

    fn conv(
        &self,
        x: &Tensor,
        name: &str,
        stride: usize,
        padding: usize,
        groups: usize,
        bias: bool,
    ) -> Result<Tensor, String> {
        let w = self.get(&format!("{name}.weight"))?;
        let mut y = x
            .conv2d(&w, padding, stride, 1, groups)
            .map_err(|e| e.to_string())?;
        if bias {
            let b = self.get(&format!("{name}.bias"))?;
            let c = b.dim(0).map_err(|e| e.to_string())?;
            y = y
                .broadcast_add(&b.reshape((1, c, 1, 1)).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        }
        Ok(y)
    }

    fn bn(&self, x: &Tensor, name: &str) -> Result<Tensor, String> {
        let mean = self.get(&format!("{name}.running_mean"))?;
        let var = self.get(&format!("{name}.running_var"))?;
        let weight = self.get(&format!("{name}.weight"))?;
        let bias = self.get(&format!("{name}.bias"))?;
        let c = mean.dim(0).map_err(|e| e.to_string())?;
        let shape = (1, c, 1, 1);
        let mean = mean.reshape(shape).map_err(|e| e.to_string())?;
        let std = (var + 1e-5)
            .map_err(|e| e.to_string())?
            .sqrt()
            .map_err(|e| e.to_string())?
            .reshape(shape)
            .map_err(|e| e.to_string())?;
        let weight = weight.reshape(shape).map_err(|e| e.to_string())?;
        let bias = bias.reshape(shape).map_err(|e| e.to_string())?;
        x.broadcast_sub(&mean)
            .map_err(|e| e.to_string())?
            .broadcast_div(&std)
            .map_err(|e| e.to_string())?
            .broadcast_mul(&weight)
            .map_err(|e| e.to_string())?
            .broadcast_add(&bias)
            .map_err(|e| e.to_string())
    }

    fn conv_bn(
        &self,
        x: &Tensor,
        conv: &str,
        bn: &str,
        stride: usize,
        padding: usize,
        groups: usize,
    ) -> Result<Tensor, String> {
        self.bn(&self.conv(x, conv, stride, padding, groups, false)?, bn)
    }
}

fn relu(x: Tensor) -> Result<Tensor, String> {
    x.relu().map_err(|e| e.to_string())
}
fn relu6(x: Tensor) -> Result<Tensor, String> {
    x.clamp(0.0f32, 6.0f32).map_err(|e| e.to_string())
}
fn gelu(x: Tensor) -> Result<Tensor, String> {
    x.gelu_erf().map_err(|e| e.to_string())
}

pub struct MarsModel {
    w: Weights,
}

impl MarsModel {
    pub fn load(path: &Path) -> Result<Self, String> {
        Ok(Self {
            w: Weights::open(path, Some("model_state_dict"), "_orig_mod.module.")?,
        })
    }

    fn double_conv(&self, x: &Tensor, name: &str) -> Result<Tensor, String> {
        let x = self.w.conv(x, &format!("{name}.0"), 1, 1, 1, true)?;
        let x = gelu(self.w.bn(&x, &format!("{name}.1"))?)?;
        let x = self.w.conv(&x, &format!("{name}.3"), 1, 1, 1, true)?;
        gelu(self.w.bn(&x, &format!("{name}.4"))?)
    }

    fn down(&self, x: &Tensor, name: &str) -> Result<Tensor, String> {
        let x = x.max_pool2d(2).map_err(|e| e.to_string())?;
        self.double_conv(&x, &format!("{name}.1"))
    }

    fn up(&self, x: &Tensor, skip: &Tensor, name: &str) -> Result<Tensor, String> {
        let (_, _, h, width) = skip.dims4().map_err(|e| e.to_string())?;
        let x = x
            .upsample_bilinear2d(h, width, true)
            .map_err(|e| e.to_string())?;
        let x = Tensor::cat(&[skip, &x], 1).map_err(|e| e.to_string())?;
        self.double_conv(&x, &format!("{name}.conv"))
    }

    pub fn predict(&self, input: &[f32], width: usize, height: usize) -> Result<Vec<f32>, String> {
        if input.len() != 16 * width * height {
            return Err("MARS input must be 16×H×W".into());
        }
        let x = Tensor::from_slice(input, (1, 16, height, width), &self.w.device)
            .map_err(|e| e.to_string())?;
        let x1 = self.double_conv(&x, "inc")?;
        let x2 = self.down(&x1, "down1")?;
        let x3 = self.down(&x2, "down2")?;
        let x4 = self.down(&x3, "down3")?;
        let x5 = self.down(&x4, "down4")?;
        let x = self.up(&x5, &x4, "up1")?;
        let x = self.up(&x, &x3, "up2")?;
        let x = self.up(&x, &x2, "up3")?;
        let x = self.up(&x, &x1, "up4")?;
        candle_nn::ops::sigmoid(&self.w.conv(&x, "out", 1, 0, 1, true)?)
            .map_err(|e| e.to_string())?
            .flatten_all()
            .map_err(|e| e.to_string())?
            .to_vec1()
            .map_err(|e| e.to_string())
    }
}

pub struct CloudModel {
    w: Weights,
}

impl CloudModel {
    pub fn load(path: &Path) -> Result<Self, String> {
        Ok(Self {
            w: Weights::open(path, None, "")?,
        })
    }

    fn feature0(&self, x: &Tensor) -> Result<Tensor, String> {
        relu6(
            self.w
                .conv_bn(x, "encoder.features.0.0", "encoder.features.0.1", 2, 1, 1)?,
        )
    }

    fn inverted(
        &self,
        x: &Tensor,
        i: usize,
        stride: usize,
        expand: bool,
        out_channels: usize,
    ) -> Result<Tensor, String> {
        let name = format!("encoder.features.{i}.conv");
        let input = x.clone();
        let mut x = x.clone();
        let depthwise;
        let project;
        if expand {
            x = relu6(self.w.conv_bn(
                &x,
                &format!("{name}.0.0"),
                &format!("{name}.0.1"),
                1,
                0,
                1,
            )?)?;
            let channels = x.dim(1).map_err(|e| e.to_string())?;
            x = relu6(self.w.conv_bn(
                &x,
                &format!("{name}.1.0"),
                &format!("{name}.1.1"),
                stride,
                1,
                channels,
            )?)?;
            depthwise = "2";
            project = "3";
        } else {
            let channels = x.dim(1).map_err(|e| e.to_string())?;
            x = relu6(self.w.conv_bn(
                &x,
                &format!("{name}.0.0"),
                &format!("{name}.0.1"),
                stride,
                1,
                channels,
            )?)?;
            depthwise = "1";
            project = "2";
        }
        x = self.w.conv_bn(
            &x,
            &format!("{name}.{depthwise}"),
            &format!("{name}.{project}"),
            1,
            0,
            1,
        )?;
        if stride == 1 && input.dim(1).map_err(|e| e.to_string())? == out_channels {
            (x + input).map_err(|e| e.to_string())
        } else {
            Ok(x)
        }
    }

    fn decoder_block(
        &self,
        x: &Tensor,
        skip: Option<&Tensor>,
        i: usize,
        h: usize,
        width: usize,
    ) -> Result<Tensor, String> {
        let mut x = x.upsample_nearest2d(h, width).map_err(|e| e.to_string())?;
        if let Some(skip) = skip {
            x = Tensor::cat(&[&x, skip], 1).map_err(|e| e.to_string())?;
        }
        let base = format!("decoder.blocks.{i}");
        x = relu(self.w.conv_bn(
            &x,
            &format!("{base}.conv1.0"),
            &format!("{base}.conv1.1"),
            1,
            1,
            1,
        )?)?;
        relu(self.w.conv_bn(
            &x,
            &format!("{base}.conv2.0"),
            &format!("{base}.conv2.1"),
            1,
            1,
            1,
        )?)
    }

    /// Predict CloudSEN classes for an already reflect-padded C×H×W tensor.
    /// Width and height must be divisible by 32.
    pub fn predict(&self, input: &[f32], width: usize, height: usize) -> Result<Vec<u8>, String> {
        if input.len() != 13 * width * height || width & 31 != 0 || height & 31 != 0 {
            return Err("CloudSEN input must be 13×H×W with H,W divisible by 32".into());
        }
        let original = Tensor::from_slice(input, (1, 13, height, width), &self.w.device)
            .map_err(|e| e.to_string())?;
        let mut x = self.feature0(&original)?;
        let mut features = Vec::with_capacity(5);
        let specs = [
            (1, 1, false, 16),
            (2, 2, true, 24),
            (3, 1, true, 24),
            (4, 2, true, 32),
            (5, 1, true, 32),
            (6, 1, true, 32),
            (7, 2, true, 64),
            (8, 1, true, 64),
            (9, 1, true, 64),
            (10, 1, true, 64),
            (11, 1, true, 96),
            (12, 1, true, 96),
            (13, 1, true, 96),
            (14, 2, true, 160),
            (15, 1, true, 160),
            (16, 1, true, 160),
            (17, 1, true, 320),
        ];
        for &(i, stride, expand, out) in &specs {
            x = self.inverted(&x, i, stride, expand, out)?;
            if matches!(i, 1 | 3 | 6 | 13) {
                features.push(x.clone());
            }
        }
        x = relu6(self.w.conv_bn(
            &x,
            "encoder.features.18.0",
            "encoder.features.18.1",
            1,
            0,
            1,
        )?)?;
        features.push(x);
        // head=1280; skips=96,32,24,16; final block upsamples without a skip.
        let mut x = features[4].clone();
        let targets = [
            features[3].dims4().unwrap(),
            features[2].dims4().unwrap(),
            features[1].dims4().unwrap(),
            features[0].dims4().unwrap(),
            original.dims4().unwrap(),
        ];
        for i in 0..5 {
            let skip = if i < 4 { Some(&features[3 - i]) } else { None };
            x = self.decoder_block(&x, skip, i, targets[i].2, targets[i].3)?;
        }
        let logits = self.w.conv(&x, "segmentation_head.0", 1, 1, 1, true)?;
        let values: Vec<f32> = logits
            .flatten_all()
            .map_err(|e| e.to_string())?
            .to_vec1()
            .map_err(|e| e.to_string())?;
        let n = width * height;
        let mut classes = vec![0u8; n];
        for i in 0..n {
            let mut best = 0;
            for c in 1..4 {
                if values[c * n + i] > values[best * n + i] {
                    best = c;
                }
            }
            classes[i] = best as u8;
        }
        Ok(classes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_paths_are_stable() {
        let p = ModelPaths::in_dir("models");
        assert_eq!(p.mars, PathBuf::from("models/mars-s2l-20250326.pt"));
        assert_eq!(MARS_SHA256.len(), 64);
        assert_eq!(CLOUD_SHA256.len(), 64);
    }

    /// Run explicitly after updating either pinned upstream checkpoint:
    /// `S2_TEST_MODELS=/path cargo test published_checkpoints_smoke -- --ignored`
    #[test]
    #[ignore]
    fn published_checkpoints_smoke() {
        let dir = PathBuf::from(std::env::var("S2_TEST_MODELS").unwrap());
        let paths = ModelPaths::in_dir(dir);
        let mars = MarsModel::load(&paths.mars).unwrap();
        let p = mars.predict(&vec![0.0; 16 * 32 * 32], 32, 32).unwrap();
        assert_eq!(p.len(), 32 * 32);
        assert!(p.iter().all(|x| x.is_finite() && (0.0..=1.0).contains(x)));
        let clouds = CloudModel::load(&paths.clouds).unwrap();
        let c = clouds.predict(&vec![0.0; 13 * 32 * 32], 32, 32).unwrap();
        assert_eq!(c.len(), 32 * 32);
        assert!(c.iter().all(|&x| x < 4));
    }

    /// Cross-runtime parity fixture generated by the published Python packages.
    #[test]
    #[ignore]
    fn published_python_parity() {
        fn f32s(path: impl AsRef<Path>) -> Vec<f32> {
            fs::read(path)
                .unwrap()
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
                .collect()
        }
        let models = ModelPaths::in_dir(std::env::var("S2_TEST_MODELS").unwrap());
        let fixture = PathBuf::from(std::env::var("S2_TEST_PARITY").unwrap());
        let actual = MarsModel::load(&models.mars)
            .unwrap()
            .predict(&f32s(fixture.join("mars-in.bin")), 32, 32)
            .unwrap();
        let expected = f32s(fixture.join("mars-out.bin"));
        let max = actual
            .iter()
            .zip(expected)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(max < 2e-5, "MARS max |Rust-PyTorch| = {max}");
        let cloud = CloudModel::load(&models.clouds)
            .unwrap()
            .predict(&f32s(fixture.join("cloud-in.bin")), 32, 32)
            .unwrap();
        assert_eq!(cloud, fs::read(fixture.join("cloud-out.bin")).unwrap());
    }
}
