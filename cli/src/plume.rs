//! L1C chip ingestion and the non-neural parts of the native MARS-S2L pipeline.

mod background;
mod chip;
mod wind;

pub use chip::{cloud_cells, component_geometry, read_chip, Chip};

use crate::models::{CloudModel, MarsModel};
use crate::stac::Item;
use chrono::{DateTime, Utc};
#[cfg(test)]
use gdal::Dataset;
use s2_flares_core::{detect_block, BlockMeta, Detection, Thresholds};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
pub fn flare_detections(chip: &Chip, item: &Item, thresholds: &Thresholds) -> Vec<Detection> {
    let (width, height) = (chip.width / 2, chip.height / 2);
    let n10 = chip.width * chip.height;
    let mut b8a = vec![0u16; width * height];
    let mut b11 = vec![0u16; width * height];
    let mut b12 = vec![0u16; width * height];
    let mut clouds = vec![0u8; width * height];
    for y in 0..height {
        for x in 0..width {
            let j = y * width + x;
            let i = y * 2 * chip.width + x * 2;
            b8a[j] = chip.nearest[8 * n10 + i];
            b11[j] = chip.nearest[11 * n10 + i];
            b12[j] = chip.nearest[12 * n10 + i];
            clouds[j] = if chip.valid[i] { 0 } else { 9 };
        }
    }
    let meta = BlockMeta {
        date: item.date.clone(),
        epsg: item.epsg,
        img_min_x: chip.min_x,
        img_max_y: chip.max_y,
        res_x: 20.0,
        res_y: 20.0,
        block_offset_x: 0,
        block_offset_y: 0,
        width,
        height,
        mgrs: item.mgrs.clone(),
        scene: item.id.clone(),
        sun_elevation: item.sun_elevation,
        sun_azimuth: item.sun_azimuth,
        radiometric_offset: 0.0,
        quantification: 10000.0,
    };
    detect_block(&b12, &b11, Some(&b8a), Some(&clouds), &meta, thresholds).0
}

#[derive(Clone, Debug)]
pub struct PlumeDetection {
    pub pixels: usize,
    pub flux_rate: f64,
    pub flux_rate_std: f64,
    pub max_probability: f32,
    /// excess-probability-weighted centroid (lon, lat): weights of p − threshold
    /// discount the faint downwind tail, so the point lands on the visible core
    /// rather than an envelope midpoint the tail would drag downwind.
    pub centre: [f64; 2],
    pub mask: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct PlumeResult {
    pub status: String,
    pub clear_percent: f32,
    pub background: Option<String>,
    pub wind: Option<[f32; 2]>,
    pub scene_score: Option<f32>,
    pub plumes: Vec<PlumeDetection>,
    pub probability: Option<Vec<f32>>,
}

impl PlumeResult {
    pub fn is_plume(&self) -> bool {
        !self.plumes.is_empty()
    }
}

/// Persist the georeferenced continuous output for a positive detection. CSV is
/// the catalogue; this compact GeoTIFF is the auditable pixel-level record.
pub fn save_probability(path: &Path, chip: &Chip, result: &PlumeResult) -> Result<(), String> {
    let values = result
        .probability
        .as_ref()
        .ok_or_else(|| "positive has no probability raster".to_string())?;
    if values.len() != chip.width * chip.height {
        return Err("probability raster shape mismatch".into());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let name = path
        .file_name()
        .ok_or_else(|| "probability output has no filename".to_string())?
        .to_string_lossy();
    let part = path.with_file_name(format!(".{name}.part"));
    let driver = gdal::DriverManager::get_driver_by_name("GTiff").map_err(|e| e.to_string())?;
    let mut dataset = driver
        .create_with_band_type::<f32, _>(&part, chip.width, chip.height, 1)
        .map_err(|e| e.to_string())?;
    dataset
        .set_geo_transform(&[chip.min_x, 10.0, 0.0, chip.max_y, 0.0, -10.0])
        .map_err(|e| e.to_string())?;
    let spatial =
        gdal::spatial_ref::SpatialRef::from_epsg(chip.epsg as u32).map_err(|e| e.to_string())?;
    dataset
        .set_spatial_ref(&spatial)
        .map_err(|e| e.to_string())?;
    {
        let mut band = dataset.rasterband(1).map_err(|e| e.to_string())?;
        let mut buffer = gdal::raster::Buffer::new((chip.width, chip.height), values.clone());
        band.write((0, 0), (chip.width, chip.height), &mut buffer)
            .map_err(|e| e.to_string())?;
        band.set_no_data_value(Some(0.0))
            .map_err(|e| e.to_string())?;
    }
    dataset.flush_cache().map_err(|e| e.to_string())?;
    drop(dataset);
    fs::rename(&part, path)
        .map_err(|e| format!("commit {} → {}: {e}", part.display(), path.display()))
}

fn xml_number_after(xml: &str, marker: &str, tag: &str) -> Option<f64> {
    let section = &xml[xml.find(marker)?..];
    let node = &section[section.find(&format!("<{tag}"))?..];
    let start = node.find('>')? + 1;
    let end = node[start..].find('<')? + start;
    node[start..end].trim().parse().ok()
}

fn acquisition_angles(item: &Item) -> Result<(f64, f64), String> {
    let url = item
        .bands
        .granule_metadata
        .as_ref()
        .ok_or_else(|| format!("{} lacks granule angle metadata", item.id))?;
    let bytes = crate::read::vsi_read(url)?;
    let xml = String::from_utf8_lossy(&bytes);
    let sza = xml_number_after(&xml, "<Mean_Sun_Angle>", "ZENITH_ANGLE")
        .ok_or_else(|| format!("{} lacks solar zenith", item.id))?;
    let vza = xml_number_after(
        &xml,
        "<Mean_Viewing_Incidence_Angle bandId=\"12\">",
        "ZENITH_ANGLE",
    )
    .ok_or_else(|| format!("{} lacks B12 viewing zenith", item.id))?;
    Ok((sza, vza))
}

fn lut_rows(value: &serde_json::Value, key: &str) -> Result<Vec<Vec<f64>>, String> {
    value[key]
        .as_array()
        .ok_or_else(|| format!("methane LUT lacks {key}"))?
        .iter()
        .map(|row| {
            row.as_array()
                .ok_or_else(|| format!("methane LUT {key} row is not an array"))?
                .iter()
                .map(|x| {
                    x.as_f64()
                        .ok_or_else(|| format!("methane LUT {key} has a non-number"))
                })
                .collect()
        })
        .collect()
}

fn lut_vector(value: &serde_json::Value, key: &str) -> Result<Vec<f64>, String> {
    value[key]
        .as_array()
        .ok_or_else(|| format!("methane LUT lacks {key}"))?
        .iter()
        .map(|x| {
            x.as_f64()
                .ok_or_else(|| format!("methane LUT {key} has a non-number"))
        })
        .collect()
}

fn lut_scalar_rows(value: &serde_json::Value, key: &str) -> Result<Vec<f64>, String> {
    value[key]
        .as_array()
        .ok_or_else(|| format!("methane LUT lacks {key}"))?
        .iter()
        .map(|row| {
            row.as_f64()
                .or_else(|| {
                    row.as_array()
                        .and_then(|x| x.first())
                        .and_then(|x| x.as_f64())
                })
                .ok_or_else(|| format!("methane LUT {key} row is not a scalar"))
        })
        .collect()
}

fn transmittance_lut(satellite: &str) -> Result<s2_flares_core::plume::TransmittanceLut, String> {
    let root: serde_json::Value =
        serde_json::from_str(include_str!("../assets/integrated_transmittances.json"))
            .map_err(|e| format!("parse methane LUT: {e}"))?;
    let sat = &root[satellite];
    if sat.is_null() {
        return Err(format!("methane LUT lacks {satellite}"));
    }
    Ok(s2_flares_core::plume::TransmittanceLut {
        amf: lut_vector(&root, "amf_arr")?,
        methane: lut_rows(&root, "mr_ch4_arr")?,
        b12: lut_rows(sat, "transmittance_b12")?,
        b11: lut_rows(sat, "transmittance_b11")?,
        b12_background: lut_scalar_rows(sat, "transmittance_b12_bg")?,
        b11_background: lut_scalar_rows(sat, "transmittance_b11_bg")?,
        background_concentration: root["background_concentration"]
            .as_f64()
            .ok_or_else(|| "methane LUT lacks background concentration".to_string())?,
    })
}

fn acquisition(item: &Item) -> Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc3339(&item.datetime)
        .map(|x| x.with_timezone(&Utc))
        .map_err(|e| format!("bad STAC datetime {}: {e}", item.datetime))
}

pub struct PlumeDetector<'a> {
    clouds: &'a CloudModel,
    mars: &'a MarsModel,
    model_dir: &'a Path,
    fixed_wind: Option<[f32; 2]>,
}

impl<'a> PlumeDetector<'a> {
    pub fn new(
        clouds: &'a CloudModel,
        mars: &'a MarsModel,
        model_dir: &'a Path,
        fixed_wind: Option<[f32; 2]>,
    ) -> Self {
        Self {
            clouds,
            mars,
            model_dir,
            fixed_wind,
        }
    }

    pub fn detect(
        &self,
        target: &Item,
        candidates: &[Item],
        lon: f64,
        lat: f64,
        cache: &mut HashMap<String, Chip>,
    ) -> Result<(Chip, PlumeResult), String> {
        let target_chip = background::cached(cache, target, lon, lat, self.clouds)?.clone();
        let mut result = PlumeResult {
            status: "ok".into(),
            clear_percent: target_chip.clear_percent,
            background: None,
            wind: None,
            scene_score: None,
            plumes: Vec::new(),
            probability: None,
        };
        if target_chip.clear_percent < 50.0 {
            result.status = "cloudy".into();
            return Ok((target_chip, result));
        }
        let zeros = target_chip.nearest.iter().filter(|&&v| v == 0).count();
        if zeros * 2 > target_chip.nearest.len() {
            result.status = "out_of_swath".into();
            return Ok((target_chip, result));
        }
        let background_id = match background::select(
            target,
            &target_chip,
            candidates,
            lon,
            lat,
            self.clouds,
            cache,
        )? {
            Some(id) => id,
            None => {
                result.status = "no_background".into();
                return Ok((target_chip, result));
            }
        };
        let background = cache.get(&background_id).unwrap();
        let wind = match self.fixed_wind {
            Some(w) => w,
            None => wind::sample(self.model_dir, acquisition(target)?, lon, lat)?,
        };
        let target_bands = target_chip.model_bands();
        let background_bands = background.model_bands();
        // Once the six model bands are selected the upstream default registration
        // channels are [3,2,1] (B08/B04/B03), so retain that slightly unusual but
        // checkpoint-compatible choice.
        let (aligned_background, _) = s2_flares_core::plume::align_background(
            &target_bands,
            &background_bands,
            target_chip.width,
            target_chip.height,
            [3, 2, 1],
        )?;
        let input = s2_flares_core::plume::model_input(
            &target_bands,
            &aligned_background,
            &target_chip.valid,
            wind,
        )?;
        let mut probability = self
            .mars
            .predict(&input, target_chip.width, target_chip.height)?;
        // predict_continuous applies the clear-pixel mask after the network.
        for (p, &valid) in probability.iter_mut().zip(&target_chip.valid) {
            if !valid {
                *p = 0.0;
            }
        }
        let components = s2_flares_core::plume::connected_components(
            &probability,
            target_chip.width,
            target_chip.height,
            s2_flares_core::plume::DEFAULT_THRESHOLD,
            s2_flares_core::plume::DEFAULT_MIN_PIXELS,
        );
        // Published code applies `> threshold_pixels` after component retention.
        let components: Vec<_> = components
            .into_iter()
            .filter(|component| component.len() > s2_flares_core::plume::DEFAULT_MIN_PIXELS)
            .collect();
        let mut mask = vec![0u8; probability.len()];
        for component in &components {
            for &i in component {
                mask[i] = 1;
            }
        }
        let score = s2_flares_core::plume::scene_score(
            &probability,
            target_chip.width,
            target_chip.height,
            s2_flares_core::plume::DEFAULT_MIN_PIXELS,
        );
        result.background = Some(background_id);
        result.wind = Some(wind);
        result.scene_score = Some(score);
        if !components.is_empty() {
            // Quantification deliberately re-registers the full 13-band background:
            // the upstream routine's default [3,2,1] channels are B04/B03/B02 here.
            let (quant_background, _) = s2_flares_core::plume::align_background(
                &target_chip.values,
                &background.values,
                target_chip.width,
                target_chip.height,
                [3, 2, 1],
            )?;
            let ratio = s2_flares_core::plume::retrieval_ratio(
                &target_chip.values,
                &quant_background,
                &target_chip.valid,
                &mask,
                11,
                12,
            )?;
            let (sza, vza) = acquisition_angles(target)?;
            let satellite = target.id.get(..3).unwrap_or("");
            let lut = transmittance_lut(satellite)?;
            let enhancement =
                s2_flares_core::plume::methane_enhancement(&ratio, satellite, sza, vza, &lut)?;
            let speed = (wind[0] as f64).hypot(wind[1] as f64);
            let (zone, north) = s2_flares_core::utm_params(target_chip.epsg);
            for component in components {
                let mut component_mask = vec![0u8; mask.len()];
                let (mut sx, mut sy, mut sp, mut max_probability) = (0.0, 0.0, 0.0, 0f32);
                for &i in &component {
                    component_mask[i] = 1;
                    max_probability = max_probability.max(probability[i]);
                    // strictly positive: component pixels are strictly above threshold
                    let w = (probability[i] - s2_flares_core::plume::DEFAULT_THRESHOLD) as f64;
                    sx += w * ((i % target_chip.width) as f64 + 0.5);
                    sy += w * ((i / target_chip.width) as f64 + 0.5);
                    sp += w;
                }
                let (flux_rate, flux_rate_std) =
                    s2_flares_core::plume::flux_rate(&enhancement, &component_mask, speed)?;
                let (lon, lat) = s2_flares_core::utm_to_wgs84(
                    target_chip.min_x + 10.0 * sx / sp,
                    target_chip.max_y - 10.0 * sy / sp,
                    zone,
                    north,
                );
                result.plumes.push(PlumeDetection {
                    pixels: component.len(),
                    flux_rate,
                    flux_rate_std,
                    max_probability,
                    centre: [lon, lat],
                    mask: component_mask,
                });
            }
        }
        result.probability = Some(probability);
        Ok((target_chip, result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reflect_padding_matches_numpy_shape() {
        assert_eq!(
            (0..7)
                .map(|i| chip::reflect_index(i - 2, 3))
                .collect::<Vec<_>>(),
            vec![2, 1, 0, 1, 2, 1, 0]
        );
    }
    #[test]
    fn identical_chips_have_zero_distance() {
        let c = Chip {
            values: vec![100.0; 13 * 4],
            nearest: vec![100; 13 * 4],
            valid: vec![true; 4],
            width: 2,
            height: 2,
            min_x: 0.0,
            max_y: 0.0,
            epsg: 32631,
            clear_percent: 100.0,
        };
        assert_eq!(background::similarity(&c, &c), 0.0);
    }

    #[test]
    fn ranking_registration_is_identical_when_unused_bands_are_omitted() {
        let (width, height) = (20, 20);
        let n = width * height;
        let mut reference = vec![0.0f32; 13 * n];
        let mut moving = vec![0.0f32; 13 * n];
        for c in 0..13 {
            for y in 0..height {
                for x in 0..width {
                    reference[c * n + y * width + x] =
                        c as f32 * 1000.0 + (x * x + 3 * y + x * y) as f32;
                    let sx = (x + 1).min(width - 1);
                    moving[c * n + y * width + x] =
                        c as f32 * 1000.0 + (sx * sx + 3 * y + sx * y) as f32;
                }
            }
        }
        let bands = [1usize, 2, 3, 11];
        let selected = |values: &[f32]| {
            bands
                .iter()
                .flat_map(|&c| values[c * n..(c + 1) * n].iter().copied())
                .collect::<Vec<_>>()
        };
        let (full, full_shift) =
            s2_flares_core::plume::align_background(&reference, &moving, width, height, [3, 2, 1])
                .unwrap();
        let (compact, compact_shift) = s2_flares_core::plume::align_background(
            &selected(&reference),
            &selected(&moving),
            width,
            height,
            [2, 1, 0],
        )
        .unwrap();
        assert_eq!(compact_shift, full_shift);
        for (j, c) in bands.into_iter().enumerate() {
            assert_eq!(&compact[j * n..(j + 1) * n], &full[c * n..(c + 1) * n]);
        }
    }

    #[test]
    fn relaxed_background_ranking_still_scores_only_the_first_twenty() {
        let (width, height) = (20, 20);
        let n = width * height;
        let mut values = vec![0.0f32; 13 * n];
        for c in 0..13 {
            for i in 0..n {
                values[c * n + i] = 1000.0 + c as f32 * 100.0 + (i % 37) as f32;
            }
        }
        let target = Chip {
            values,
            nearest: vec![1000; 13 * n],
            valid: vec![true; n],
            width,
            height,
            min_x: 0.0,
            max_y: 0.0,
            epsg: 32631,
            clear_percent: 100.0,
        };
        let mut ids = Vec::new();
        let mut cache = HashMap::new();
        for j in 0..=background::LIMIT {
            let id = format!("background-{j:02}");
            let mut background = target.clone();
            if j < background::LIMIT {
                background.values[11 * n + j] += 100.0 + j as f32;
            }
            ids.push(id.clone());
            cache.insert(id, background);
        }

        let chosen = background::most_similar(&target, &ids, &cache).unwrap();

        assert_ne!(chosen, ids[background::LIMIT]);
        assert!(ids[..background::LIMIT].contains(&chosen));
    }

    #[test]
    fn published_quantification_lut_parity() {
        let ratio = [0.3, 0.5, 0.7, 0.9, 1.0, 1.05, 1.08];
        let lut = transmittance_lut("S2B").unwrap();
        let methane =
            s2_flares_core::plume::methane_enhancement(&ratio, "S2B", 35.2, 4.7, &lut).unwrap();
        let python = [
            521963.06451834086,
            224366.3522218766,
            70150.51774730693,
            10095.281616309723,
            0.0,
            -2552.1036535896833,
            -3406.4511351694177,
        ];
        for (rust, reference) in methane.iter().zip(python) {
            assert!(
                (rust - reference).abs() < 0.2,
                "rust={rust} python={reference}"
            );
        }
        let (q, sigma) =
            s2_flares_core::plume::flux_rate(&methane, &[1, 1, 1, 1, 0, 1, 1], 3.2).unwrap();
        assert!((q - 35381.290492678956).abs() < 0.01, "{q}");
        // Closed-form propagation converges to the upstream Monte Carlo value
        // without retaining its sampling noise.
        assert!((sigma - 12406.8).abs() / 12406.8 < 0.01, "{sigma}");
    }

    #[test]
    fn probability_raster_is_committed_atomically() {
        let root = std::env::temp_dir().join(format!(
            "s2-flares-probability-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = root.join("probability.tif");
        let chip = Chip {
            values: vec![0.0; 13 * 4],
            nearest: vec![0; 13 * 4],
            valid: vec![true; 4],
            width: 2,
            height: 2,
            min_x: 500_000.0,
            max_y: 5_800_000.0,
            epsg: 32631,
            clear_percent: 100.0,
        };
        let result = PlumeResult {
            status: "ok".into(),
            clear_percent: 100.0,
            background: None,
            wind: None,
            scene_score: Some(1.0),
            plumes: vec![PlumeDetection {
                pixels: 1,
                flux_rate: 0.0,
                flux_rate_std: 0.0,
                max_probability: 1.0,
                centre: [0.0, 0.0],
                mask: vec![0, 0, 0, 1],
            }],
            probability: Some(vec![0.0, 0.25, 0.5, 1.0]),
        };

        save_probability(&path, &chip, &result).unwrap();

        assert!(path.exists());
        assert!(!root.join(".probability.tif.part").exists());
        let raster = Dataset::open(&path).unwrap();
        assert_eq!(raster.raster_size(), (2, 2));
        assert_eq!(
            raster.geo_transform().unwrap(),
            [500_000.0, 10.0, 0.0, 5_800_000.0, 0.0, -10.0]
        );
        drop(raster);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore]
    fn published_aws_reader_parity() {
        crate::read::configure();
        let model_dir = std::path::PathBuf::from(std::env::var("S2_TEST_MODELS").unwrap());
        let fixture = std::path::PathBuf::from(std::env::var("S2_TEST_REAL_PREPROCESS").unwrap());
        let cloud = CloudModel::load(&model_dir.join("cloudsen12-v2.pt")).unwrap();
        let item = crate::stac::search(
            [53.79962, 39.35872, 53.79962, 39.35872],
            "2024-10-25",
            "2024-10-25",
            100.0,
            "aws-l1c",
        )
        .unwrap()
        .into_iter()
        .find(|x| x.mgrs == "40SBJ")
        .unwrap();
        let chip = read_chip(&item, 53.79962, 39.35872, &cloud).unwrap();
        let expected_u16: Vec<u16> = std::fs::read(fixture.join("cloud-real-in-u16.bin"))
            .unwrap()
            .chunks_exact(2)
            .map(|x| u16::from_le_bytes(x.try_into().unwrap()))
            .collect();
        assert_eq!(chip.nearest.len(), expected_u16.len());
        let exact = chip
            .nearest
            .iter()
            .zip(&expected_u16)
            .filter(|(a, b)| a == b)
            .count() as f64
            / expected_u16.len() as f64;
        let mad = chip
            .nearest
            .iter()
            .zip(&expected_u16)
            .map(|(a, b)| (*a as f64 - *b as f64).abs())
            .sum::<f64>()
            / expected_u16.len() as f64;
        let expected_f32: Vec<f32> = std::fs::read(fixture.join("mars-real-target.bin"))
            .unwrap()
            .chunks_exact(4)
            .map(|x| f32::from_le_bytes(x.try_into().unwrap()))
            .collect();
        let actual = chip.model_bands();
        let model_mad = actual
            .iter()
            .zip(&expected_f32)
            .map(|(a, b)| (*a - *b).abs() as f64)
            .sum::<f64>()
            / actual.len() as f64;
        let n = chip.width * chip.height;
        let per_band: Vec<(f64, f32)> = (0..6)
            .map(|c| {
                let diffs: Vec<f32> = actual[c * n..(c + 1) * n]
                    .iter()
                    .zip(&expected_f32[c * n..(c + 1) * n])
                    .map(|(a, b)| (*a - *b).abs())
                    .collect();
                (
                    diffs.iter().map(|&x| x as f64).sum::<f64>() / n as f64,
                    diffs.into_iter().fold(0.0, f32::max),
                )
            })
            .collect();
        eprintln!(
            "reader exact={:.3}% nearest_mad={mad:.3} model_mad={model_mad:.3} per_band={per_band:?} clear={}",
            exact * 100.0,
            chip.clear_percent
        );
        assert!(mad < 1.0 && model_mad < 0.1);
    }
}
