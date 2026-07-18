//! Pure MARS-S2L tensor preparation and scene post-processing.
//!
//! The neural network and imagery I/O live in the native CLI.  Keeping these
//! operations here pins the published model contract without pulling an ML
//! runtime into the WASM flare detector.

/// Published MARS-S2L Sentinel-2 band order.
pub const MODEL_BANDS: [&str; 6] = ["B02", "B03", "B04", "B08", "B11", "B12"];
pub const INPUT_CHANNELS: usize = 16;
pub const DEFAULT_THRESHOLD: f32 = 0.5;
pub const DEFAULT_MIN_PIXELS: usize = 100;

/// Estimate the small translation between two acquisitions and warp the moving
/// image onto the reference grid. MARS-S2L's published preprocessing uses a
/// translation-only registration over the central 80% of three visible/NIR
/// channels, rejects shifts above five pixels, and applies bilinear interpolation
/// with replicated borders. This is the same contract without a computer-vision
/// runtime.
pub fn align_background(
    reference: &[f32],
    moving: &[f32],
    width: usize,
    height: usize,
    channels: [usize; 3],
) -> Result<(Vec<f32>, [f32; 2]), String> {
    let n = width * height;
    if n == 0 || reference.len() != moving.len() || reference.len().checked_rem(n) != Some(0) {
        return Err("co-registration tensor shape mismatch".into());
    }
    let channel_count = reference.len() / n;
    if channels.iter().any(|&c| c >= channel_count) {
        return Err("co-registration channel is out of range".into());
    }
    let mut fixed = vec![0f32; n];
    let mut mobile = vec![0f32; n];
    for i in 0..n {
        for &c in &channels {
            fixed[i] += reference[c * n + i] / 3.0;
            mobile[i] += moving[c * n + i] / 3.0;
        }
    }

    // satalign crops by (size - round(0.8*size))/2 on each side. With
    // odd remainders the Python slice keeps the extra row/column at the end.
    let crop = ((width.min(height) as f32) * 0.8).round() as usize;
    let margin_x = (width - crop.min(width)) / 2;
    let margin_y = (height - crop.min(height)) / 2;
    let (x0, x1) = (margin_x, width - margin_x);
    let (y0, y1) = (margin_y, height - margin_y);
    let score = |dx: i32, dy: i32| -> f64 {
        let mut sum_a = 0.0f64;
        let mut sum_b = 0.0f64;
        let mut sum_aa = 0.0f64;
        let mut sum_bb = 0.0f64;
        let mut sum_ab = 0.0f64;
        let mut count = 0.0f64;
        for y in y0..y1 {
            let sy = y as i32 - dy;
            if sy < 0 || sy >= height as i32 {
                continue;
            }
            for x in x0..x1 {
                let sx = x as i32 - dx;
                if sx < 0 || sx >= width as i32 {
                    continue;
                }
                let a = fixed[y * width + x] as f64;
                let b = mobile[sy as usize * width + sx as usize] as f64;
                sum_a += a;
                sum_b += b;
                sum_aa += a * a;
                sum_bb += b * b;
                sum_ab += a * b;
                count += 1.0;
            }
        }
        let cov = sum_ab - sum_a * sum_b / count;
        let var_a = (sum_aa - sum_a * sum_a / count).max(0.0);
        let var_b = (sum_bb - sum_b * sum_b / count).max(0.0);
        if var_a == 0.0 || var_b == 0.0 {
            f64::NEG_INFINITY
        } else {
            cov / (var_a * var_b).sqrt()
        }
    };
    let mut best = (f64::NEG_INFINITY, 0i32, 0i32);
    let mut scores = [[f64::NEG_INFINITY; 11]; 11];
    for dy in -5..=5 {
        for dx in -5..=5 {
            let s = score(dx, dy);
            scores[(dy + 5) as usize][(dx + 5) as usize] = s;
            if s > best.0 {
                best = (s, dx, dy);
            }
        }
    }
    let refine = |minus: f64, centre: f64, plus: f64| -> f32 {
        let denom = minus - 2.0 * centre + plus;
        if !denom.is_finite() || denom.abs() < 1e-12 {
            0.0
        } else {
            (0.5 * (minus - plus) / denom).clamp(-0.5, 0.5) as f32
        }
    };
    let (mut tx, mut ty) = (best.1 as f32, best.2 as f32);
    let (ix, iy) = ((best.1 + 5) as usize, (best.2 + 5) as usize);
    if ix > 0 && ix < 10 {
        tx += refine(scores[iy][ix - 1], scores[iy][ix], scores[iy][ix + 1]);
    }
    if iy > 0 && iy < 10 {
        ty += refine(scores[iy - 1][ix], scores[iy][ix], scores[iy + 1][ix]);
    }
    if tx.hypot(ty) > 5.0 {
        tx = 0.0;
        ty = 0.0;
    }

    let mut aligned = vec![0f32; moving.len()];
    for c in 0..channel_count {
        for y in 0..height {
            for x in 0..width {
                let sx = x as f32 - tx;
                let sy = y as f32 - ty;
                let ax = sx.floor() as isize;
                let ay = sy.floor() as isize;
                let (wx, wy) = (sx - ax as f32, sy - ay as f32);
                let at = |xx: isize, yy: isize| {
                    moving[c * n
                        + yy.clamp(0, height as isize - 1) as usize * width
                        + xx.clamp(0, width as isize - 1) as usize]
                };
                aligned[c * n + y * width + x] = (at(ax, ay) * (1.0 - wx) + at(ax + 1, ay) * wx)
                    * (1.0 - wy)
                    + (at(ax, ay + 1) * (1.0 - wx) + at(ax + 1, ay + 1) * wx) * wy;
            }
        }
    }
    Ok((aligned, [tx, ty]))
}

/// Build the exact 16-channel tensor consumed by the operational MARS-S2L
/// checkpoint. `target` and `background` are six channel-major TOA arrays in
/// reflectance×10000 units. `valid` is true for clear pixels.
///
/// Channel order is MBMP, target×6, background×6, wind-u/8, wind-v/8, cloud.
pub fn model_input(
    target: &[f32],
    background: &[f32],
    valid: &[bool],
    wind: [f32; 2],
) -> Result<Vec<f32>, String> {
    let n = valid.len();
    if target.len() != MODEL_BANDS.len() * n || background.len() != target.len() {
        return Err(format!(
            "MARS tensor shape mismatch: target={} background={} pixels={n}",
            target.len(),
            background.len()
        ));
    }

    let mut out = vec![0.0; INPUT_CHANNELS * n];
    // The reference normalises spectral channels before computing MBMP.  The
    // common /5000 factor cancels in each B12/B11 ratio.
    for c in 0..6 {
        for i in 0..n {
            out[(1 + c) * n + i] = target[c * n + i] / 5000.0;
            out[(7 + c) * n + i] = background[c * n + i] / 5000.0;
        }
    }

    let target_ratio = normalised_ratio(&out, 1 + 4, 1 + 5, n);
    let background_ratio = normalised_ratio(&out, 7 + 4, 7 + 5, n);
    for i in 0..n {
        let v = target_ratio[i] / background_ratio[i];
        out[i] = if v.is_finite() { v.min(10.0) } else { 1.0 };
        out[13 * n + i] = wind[0] / 8.0;
        out[14 * n + i] = wind[1] / 8.0;
        out[15 * n + i] = if valid[i] { 0.0 } else { 1.0 };
    }
    Ok(out)
}

/// The small public MARS-S2L methane-transmittance lookup table after JSON
/// parsing. Rows follow `amf`; columns are methane concentrations.
#[derive(Clone, Debug)]
pub struct TransmittanceLut {
    pub amf: Vec<f64>,
    pub methane: Vec<Vec<f64>>,
    pub b12: Vec<Vec<f64>>,
    pub b11: Vec<Vec<f64>>,
    pub b12_background: Vec<f64>,
    pub b11_background: Vec<f64>,
    pub background_concentration: f64,
}

/// Recompute the Irakulis-Loitxate MBMP ratio for quantification. Unlike the
/// neural input (which follows its checkpoint's median normalization), the
/// published retrieval uses means and excludes predicted plume pixels from the
/// target normalization.
pub fn retrieval_ratio(
    target: &[f32],
    aligned_background: &[f32],
    valid: &[bool],
    plume: &[u8],
    b11: usize,
    b12: usize,
) -> Result<Vec<f64>, String> {
    let n = valid.len();
    if target.len() != aligned_background.len()
        || target.len().checked_rem(n) != Some(0)
        || plume.len() != n
    {
        return Err("retrieval tensor shape mismatch".into());
    }
    let channels = target.len() / n;
    if b11 >= channels || b12 >= channels {
        return Err("retrieval band is out of range".into());
    }
    let ratio = |image: &[f32], mask: Option<&[bool]>, exclude: Option<&[u8]>| {
        let mut out = vec![0f64; n];
        let mut sum = 0.0;
        let mut count = 0usize;
        for i in 0..n {
            let den = image[b11 * n + i] as f64;
            let value = image[b12 * n + i] as f64 / den;
            if den != 0.0 && value.is_finite() {
                out[i] = value.clamp(0.0, 10.0);
                let keep = mask.is_none_or(|m| m[i]) && exclude.is_none_or(|m| m[i] == 0);
                if keep {
                    sum += out[i];
                    count += 1;
                }
            }
        }
        let mean = if count == 0 { 1.0 } else { sum / count as f64 };
        for value in &mut out {
            if *value != 0.0 {
                *value /= mean;
            }
        }
        out
    };
    let current = ratio(target, Some(valid), Some(plume));
    // The operational call does not pass a background validity mask.
    let background = ratio(aligned_background, None, None);
    Ok(current
        .into_iter()
        .zip(background)
        .map(|(a, b)| {
            if a == 0.0 || b == 0.0 {
                1.0
            } else {
                (a / b).clamp(0.0, 10.0)
            }
        })
        .collect())
}

/// Convert MBMP ratios to methane enhancement (ppb) using the public integrated
/// transmittance LUT. Cubic interpolation uses the same not-a-knot boundary
/// condition as scipy's `interp1d(kind="cubic")`.
pub fn methane_enhancement(
    ratio: &[f64],
    satellite: &str,
    solar_zenith: f64,
    viewing_zenith: f64,
    lut: &TransmittanceLut,
) -> Result<Vec<f64>, String> {
    if !matches!(satellite, "S2A" | "S2B" | "S2C") {
        return Err(format!("unsupported methane LUT satellite {satellite}"));
    }
    let rows = lut.amf.len();
    if rows < 4
        || lut.methane.len() != rows
        || lut.b12.len() != rows
        || lut.b11.len() != rows
        || lut.b12_background.len() != rows
        || lut.b11_background.len() != rows
    {
        return Err("invalid methane transmittance LUT".into());
    }
    let columns = lut.methane[0].len();
    if columns < 4
        || lut
            .methane
            .iter()
            .chain(&lut.b12)
            .chain(&lut.b11)
            .any(|r| r.len() != columns)
    {
        return Err("invalid methane transmittance LUT shape".into());
    }
    let amf = (1.0 / viewing_zenith.to_radians().cos() + 1.0 / solar_zenith.to_radians().cos())
        .min(*lut.amf.last().unwrap());
    let along_amf = |table: &[Vec<f64>]| -> Result<Vec<f64>, String> {
        (0..columns)
            .map(|column| {
                let y: Vec<f64> = table.iter().map(|row| row[column]).collect();
                cubic(&lut.amf, &y, amf)
            })
            .collect()
    };
    let methane = along_amf(&lut.methane)?;
    let b12 = along_amf(&lut.b12)?;
    let b11 = along_amf(&lut.b11)?;
    let bg12 = cubic(&lut.amf, &lut.b12_background, amf)?;
    let bg11 = cubic(&lut.amf, &lut.b11_background, amf)?;
    let ratio_trans: Vec<f64> = b12.iter().zip(&b11).map(|(a, b)| a / b).collect();
    ratio
        .iter()
        .map(|&observed| {
            if !observed.is_finite() || observed == 1.0 {
                return Ok(0.0);
            }
            let corrected = observed.clamp(0.3, 1.08) * bg12 / bg11;
            Ok(cubic(&ratio_trans, &methane, corrected)? - lut.background_concentration)
        })
        .collect()
}

/// Integrated-mass-enhancement flux in kg/h and its uncertainty. The uncertainty
/// is the closed-form variance of the same independent normal variables used by
/// the upstream 100k-sample Monte Carlo, making the native result deterministic.
pub fn flux_rate(
    enhancement_ppb: &[f64],
    plume: &[u8],
    wind_speed: f64,
) -> Result<(f64, f64), String> {
    if enhancement_ppb.len() != plume.len() {
        return Err("flux tensor shape mismatch".into());
    }
    let pixels = plume.iter().filter(|&&x| x != 0).count();
    if pixels == 0 {
        return Ok((0.0, 0.0));
    }
    let sum_ppmxm: f64 = enhancement_ppb
        .iter()
        .zip(plume)
        .filter(|(_, m)| **m != 0)
        .map(|(&x, _)| x.clamp(-600.0, 100_000.0) * 8.0)
        .sum();
    let area = 100.0;
    let ime = sum_ppmxm * area * 1_000.0 * 0.01604 / (1e6 * 22.4);
    let length = (pixels as f64 * area).sqrt();
    let ueff = 0.33 * wind_speed + 0.45;
    let q = if sum_ppmxm > 0.0 {
        3600.0 * ueff * ime / length
    } else {
        0.0
    };

    let sigma_xch4_ppmxm = 205.64270005117675 * 8.0;
    let sigma_ime =
        sigma_xch4_ppmxm * (pixels as f64).sqrt() * area * 1_000.0 * 0.01604 / (1e6 * 22.4);
    let ea2 = 0.33f64.powi(2) + 0.01f64.powi(2);
    let ew2 = wind_speed.powi(2) * (1.0 + 0.5f64.powi(2));
    let variance_ueff = (ea2 * ew2 - (0.33 * wind_speed).powi(2)) + 0.01f64.powi(2);
    let variance_product = variance_ueff * sigma_ime.powi(2)
        + variance_ueff * ime.powi(2)
        + sigma_ime.powi(2) * ueff.powi(2);
    Ok((q, 3600.0 / length * variance_product.max(0.0).sqrt()))
}

fn cubic(x: &[f64], y: &[f64], at: f64) -> Result<f64, String> {
    if x.len() != y.len() || x.len() < 4 {
        return Err("cubic interpolation needs four points".into());
    }
    let mut pairs: Vec<(f64, f64)> = x.iter().copied().zip(y.iter().copied()).collect();
    if pairs.first().unwrap().0 > pairs.last().unwrap().0 {
        pairs.reverse();
    }
    if pairs.windows(2).any(|w| w[0].0 >= w[1].0) {
        return Err("cubic interpolation axis is not monotonic".into());
    }
    let (x, y): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();
    let n = x.len();
    let h: Vec<f64> = x.windows(2).map(|w| w[1] - w[0]).collect();
    let mut a = vec![vec![0f64; n]; n];
    let mut rhs = vec![0f64; n];
    a[0][0] = -h[1];
    a[0][1] = h[0] + h[1];
    a[0][2] = -h[0];
    for i in 1..n - 1 {
        a[i][i - 1] = h[i - 1];
        a[i][i] = 2.0 * (h[i - 1] + h[i]);
        a[i][i + 1] = h[i];
        rhs[i] = 6.0 * ((y[i + 1] - y[i]) / h[i] - (y[i] - y[i - 1]) / h[i - 1]);
    }
    a[n - 1][n - 3] = -h[n - 2];
    a[n - 1][n - 2] = h[n - 3] + h[n - 2];
    a[n - 1][n - 1] = -h[n - 3];
    for column in 0..n {
        let pivot = (column..n)
            .max_by(|&i, &j| a[i][column].abs().total_cmp(&a[j][column].abs()))
            .unwrap();
        if a[pivot][column].abs() < 1e-14 {
            return Err("singular cubic interpolation".into());
        }
        a.swap(column, pivot);
        rhs.swap(column, pivot);
        let divisor = a[column][column];
        for value in a[column].iter_mut().skip(column) {
            *value /= divisor;
        }
        rhs[column] /= divisor;
        let pivot_row = a[column].clone();
        for i in 0..n {
            if i != column {
                let factor = a[i][column];
                for (value, pivot_value) in a[i].iter_mut().zip(pivot_row.iter()).skip(column) {
                    *value -= factor * pivot_value;
                }
                rhs[i] -= factor * rhs[column];
            }
        }
    }
    let segment = if at <= x[0] {
        0
    } else if at >= x[n - 1] {
        n - 2
    } else {
        x.partition_point(|&v| v <= at).saturating_sub(1).min(n - 2)
    };
    let span = h[segment];
    let left = x[segment + 1] - at;
    let right = at - x[segment];
    Ok(rhs[segment] * left.powi(3) / (6.0 * span)
        + rhs[segment + 1] * right.powi(3) / (6.0 * span)
        + (y[segment] - rhs[segment] * span.powi(2) / 6.0) * left / span
        + (y[segment + 1] - rhs[segment + 1] * span.powi(2) / 6.0) * right / span)
}

fn normalised_ratio(data: &[f32], b11: usize, b12: usize, n: usize) -> Vec<f32> {
    let mut ratio = vec![1.0; n];
    let mut values = Vec::with_capacity(n);
    for i in 0..n {
        let den = data[b11 * n + i];
        if den != 0.0 {
            let v = data[b12 * n + i] / den;
            ratio[i] = v;
            values.push(v);
        }
    }
    // torch.median returns the lower middle element for an even-length vector.
    let median = lower_median(&mut values).unwrap_or(1.0);
    for v in &mut ratio {
        *v = (*v / median).clamp(0.0, 10.0);
    }
    ratio
}

fn lower_median(values: &mut [f32]) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    let k = (values.len() - 1) / 2;
    values.select_nth_unstable_by(k, |a, b| a.total_cmp(b));
    Some(values[k])
}

/// Retain 8-connected components strictly above `threshold` with at least
/// `min_pixels` pixels. This matches `binary_connected_prediction`.
pub fn connected_mask(
    probability: &[f32],
    width: usize,
    height: usize,
    threshold: f32,
    min_pixels: usize,
) -> Vec<u8> {
    let mut out = vec![0; probability.len()];
    for component in connected_components(probability, width, height, threshold, min_pixels) {
        for i in component {
            out[i] = 1;
        }
    }
    out
}

/// Pixel indices for every retained 8-connected component. Keeping component
/// identity lets callers emit and quantify distinct plumes instead of collapsing
/// a scene's complete mask into one aggregate observation.
pub fn connected_components(
    probability: &[f32],
    width: usize,
    height: usize,
    threshold: f32,
    min_pixels: usize,
) -> Vec<Vec<usize>> {
    assert_eq!(probability.len(), width * height);
    let mut seen = vec![false; probability.len()];
    let mut out = Vec::new();
    let mut stack = Vec::new();
    let mut component = Vec::new();
    for seed in 0..probability.len() {
        if seen[seed] || probability[seed] <= threshold {
            continue;
        }
        seen[seed] = true;
        stack.push(seed);
        component.clear();
        while let Some(i) = stack.pop() {
            component.push(i);
            let r = i / width;
            let c = i % width;
            for dr in -1isize..=1 {
                for dc in -1isize..=1 {
                    if dr == 0 && dc == 0 {
                        continue;
                    }
                    let (rr, cc) = (r as isize + dr, c as isize + dc);
                    if rr < 0 || cc < 0 || rr >= height as isize || cc >= width as isize {
                        continue;
                    }
                    let j = rr as usize * width + cc as usize;
                    if !seen[j] && probability[j] > threshold {
                        seen[j] = true;
                        stack.push(j);
                    }
                }
            }
        }
        if component.len() >= min_pixels {
            out.push(component.clone());
        }
    }
    out
}

/// Published scene score: the highest probability cutoff that still contains
/// a connected component of `min_pixels`, found to 1e-3 tolerance.
pub fn scene_score(probability: &[f32], width: usize, height: usize, min_pixels: usize) -> f32 {
    if probability.is_empty() {
        return 0.0;
    }
    let mut lo = probability.iter().copied().fold(f32::INFINITY, f32::min);
    let mut hi = probability
        .iter()
        .copied()
        .fold(f32::NEG_INFINITY, f32::max);
    while hi - lo > 1e-3 {
        let mid = (lo + hi) * 0.5;
        let n = connected_mask(probability, width, height, mid, min_pixels)
            .into_iter()
            .map(usize::from)
            .sum::<usize>();
        if n >= min_pixels {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (lo + hi) * 0.5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_contract_and_mbmp() {
        let n = 4;
        let mut a = vec![1000.0; 6 * n];
        let b = vec![1000.0; 6 * n];
        a[5 * n] = 900.0;
        let x = model_input(&a, &b, &[true, true, false, true], [8.0, -4.0]).unwrap();
        assert_eq!(x.len(), 16 * n);
        assert_eq!(&x[13 * n..14 * n], &[1.0; 4]);
        assert_eq!(&x[14 * n..15 * n], &[-0.5; 4]);
        assert_eq!(&x[15 * n..], &[0.0, 0.0, 1.0, 0.0]);
        assert!(x[0] < x[1]);
    }

    #[test]
    fn connected_components_are_eight_connected() {
        let p = [0.9, 0.0, 0.0, 0.9];
        assert_eq!(connected_mask(&p, 2, 2, 0.5, 2), vec![1, 0, 0, 1]);
        assert_eq!(connected_mask(&p, 2, 2, 0.5, 3), vec![0; 4]);
        assert_eq!(connected_components(&p, 2, 2, 0.5, 2), vec![vec![0, 3]]);
    }

    #[test]
    fn score_tracks_large_component() {
        let mut p = vec![0.0; 100];
        p[..20].fill(0.73);
        let s = scene_score(&p, 10, 10, 20);
        assert!((s - 0.73).abs() < 0.002, "{s}");
    }

    #[test]
    fn registration_recovers_translation() {
        let (w, h) = (32, 32);
        let n = w * h;
        let mut fixed = vec![0f32; 3 * n];
        for c in 0..3 {
            for y in 4..28 {
                for x in 4..28 {
                    fixed[c * n + y * w + x] = ((x * 7 + y * 11 + x * y) % 97) as f32;
                }
            }
        }
        let mut moving = vec![0f32; 3 * n];
        for c in 0..3 {
            for y in 0..h {
                for x in 0..w {
                    let sx = (x as isize - 2).clamp(0, w as isize - 1) as usize;
                    let sy = (y as isize + 1).clamp(0, h as isize - 1) as usize;
                    moving[c * n + y * w + x] = fixed[c * n + sy * w + sx];
                }
            }
        }
        let (_, shift) = align_background(&fixed, &moving, w, h, [0, 1, 2]).unwrap();
        assert!((shift[0] + 2.0).abs() < 0.3, "{shift:?}");
        assert!((shift[1] - 1.0).abs() < 0.3, "{shift:?}");
    }

    /// Optional real-scene fixture generated by the published Python package.
    #[test]
    #[ignore]
    fn published_registration_parity() {
        fn values(name: &str) -> Vec<f32> {
            let dir = std::env::var("S2_TEST_REAL_PREPROCESS").unwrap();
            std::fs::read(std::path::Path::new(&dir).join(name))
                .unwrap()
                .chunks_exact(4)
                .map(|x| f32::from_le_bytes(x.try_into().unwrap()))
                .collect()
        }
        let target = values("mars-real-target.bin");
        let background = values("mars-real-background.bin");
        let expected = values("mars-real-tensor.bin");
        let (aligned, shift) = align_background(&target, &background, 210, 204, [3, 2, 1]).unwrap();
        let actual =
            model_input(&target, &aligned, &vec![true; 210 * 204], [2.621, -1.553]).unwrap();
        let mean = actual
            .iter()
            .zip(&expected)
            .map(|(a, b)| (a - b).abs() as f64)
            .sum::<f64>()
            / actual.len() as f64;
        let max = actual
            .iter()
            .zip(&expected)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        eprintln!("shift={shift:?} mean={mean} max={max}");
        assert!(mean < 2e-3, "registration tensor mean |Rust-Python|={mean}");
    }
}
