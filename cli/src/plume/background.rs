//! Published temporal background search and registered four-band ranking.

use super::{acquisition, read_chip, Chip};
use crate::models::CloudModel;
use crate::stac::Item;
use rayon::prelude::*;
use std::collections::HashMap;

const BATCH: usize = 4;
pub(super) const LIMIT: usize = 20;

/// Four-band brightness-normalised absolute difference used to rank the first
/// 20 clear background candidates (B02/B03/B04/B11).
pub fn similarity(target: &Chip, background: &Chip) -> f32 {
    if target.width != background.width || target.height != background.height {
        return f32::INFINITY;
    }
    let n = target.width * target.height;
    let bands = [1usize, 2, 3, 11];
    let selected = |chip: &Chip| {
        bands
            .iter()
            .flat_map(|&channel| chip.values[channel * n..(channel + 1) * n].iter().copied())
            .collect::<Vec<_>>()
    };
    let target_bands = selected(target);
    let background_bands = selected(background);
    let aligned = match s2_flares_core::plume::align_background(
        &target_bands,
        &background_bands,
        target.width,
        target.height,
        [2, 1, 0],
    ) {
        Ok((image, _)) => image,
        Err(_) => return f32::INFINITY,
    };
    let mut means_t = [0.0f64; 4];
    let mut means_b = [0.0f64; 4];
    let nt = target.valid.iter().filter(|&&x| x).count().max(1) as f64;
    let nb = background.valid.iter().filter(|&&x| x).count().max(1) as f64;
    for j in 0..4 {
        for i in 0..n {
            if target.valid[i] {
                means_t[j] += target_bands[j * n + i] as f64 / 10000.0;
            }
            if background.valid[i] {
                means_b[j] += aligned[j * n + i] as f64 / 10000.0;
            }
        }
        means_t[j] /= nt;
        means_b[j] /= nb;
    }
    let mut sum = 0.0f64;
    let mut count = 0usize;
    for i in 0..n {
        if !(target.valid[i] && background.valid[i]) {
            continue;
        }
        let mut difference = 0.0;
        for j in 0..4 {
            let scale = if means_b[j] == 0.0 {
                1.0
            } else {
                means_t[j] / means_b[j]
            };
            difference += (target_bands[j * n + i] as f64 / 10000.0
                - aligned[j * n + i] as f64 / 10000.0 * scale)
                .abs();
        }
        sum += difference / 4.0;
        count += 1;
    }
    if count == 0 {
        f32::INFINITY
    } else {
        (sum / count as f64) as f32
    }
}

pub(super) fn most_similar(
    target: &Chip,
    candidates: &[String],
    cache: &HashMap<String, Chip>,
) -> Option<String> {
    candidates
        .iter()
        .take(LIMIT)
        .filter_map(|id| cache.get(id).map(|chip| (id, similarity(target, chip))))
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(id, _)| id.clone())
}

fn chip<'a>(
    cache: &'a mut HashMap<String, Chip>,
    item: &Item,
    lon: f64,
    lat: f64,
    clouds: &CloudModel,
) -> Result<&'a Chip, String> {
    if !cache.contains_key(&item.id) {
        cache.insert(item.id.clone(), read_chip(item, lon, lat, clouds)?);
    }
    Ok(cache.get(&item.id).unwrap())
}

pub(super) fn select(
    target: &Item,
    target_chip: &Chip,
    candidates: &[Item],
    lon: f64,
    lat: f64,
    clouds: &CloudModel,
    cache: &mut HashMap<String, Chip>,
) -> Result<Option<String>, String> {
    let target_time = acquisition(target)?;
    let mut order: Vec<&Item> = candidates
        .iter()
        .filter(|candidate| candidate.id != target.id)
        .filter(|candidate| candidate.cloud_cover.unwrap_or(100.0) <= 95.0)
        .collect();
    order.sort_by_key(|candidate| {
        acquisition(candidate)
            .map(|time| (time - target_time).num_seconds().abs())
            .unwrap_or(i64::MAX)
    });
    let mut strict = Vec::new();
    let mut loose = Vec::new();
    let mut eligible = Vec::with_capacity(order.len());
    for candidate in order {
        let delta = (acquisition(candidate)? - target_time).num_seconds().abs();
        if (300..=120 * 86400).contains(&delta) {
            eligible.push(candidate);
        }
    }
    let mut inspected = 0usize;
    'batches: for (batch_index, batch) in eligible.chunks(BATCH).enumerate() {
        let missing: Vec<&Item> = batch
            .iter()
            .copied()
            .filter(|candidate| !cache.contains_key(&candidate.id))
            .collect();
        let loaded_any = !missing.is_empty();
        let loaded: Vec<(&Item, Result<Chip, String>)> = missing
            .par_iter()
            .map(|candidate| (*candidate, read_chip(candidate, lon, lat, clouds)))
            .collect();
        for (candidate, result) in loaded {
            match result {
                Ok(chip) => {
                    cache.insert(candidate.id.clone(), chip);
                }
                Err(error) => eprintln!("    background {} skipped: {error}", candidate.id),
            }
        }
        let mut limit_reached = false;
        for candidate in batch {
            let Some(background) = cache.get(&candidate.id) else {
                continue;
            };
            if background.clear_percent >= 95.0 {
                strict.push(candidate.id.clone());
            }
            if background.clear_percent >= 65.0 {
                loose.push(candidate.id.clone());
            }
            if strict.len() >= LIMIT {
                limit_reached = true;
                break;
            }
        }
        inspected += batch.len();
        if loaded_any
            && ((batch_index + 1) % 5 == 0 || limit_reached || inspected == eligible.len())
        {
            eprintln!(
                "    {} backgrounds: {inspected}/{} inspected · {} >=95% clear · {} >=65% clear",
                target.id,
                eligible.len(),
                strict.len(),
                loose.len()
            );
        }
        if limit_reached {
            break 'batches;
        }
    }
    let usable = if strict.is_empty() { &loose } else { &strict };
    Ok(most_similar(target_chip, usable, cache))
}

pub(super) fn cached<'a>(
    cache: &'a mut HashMap<String, Chip>,
    item: &Item,
    lon: f64,
    lat: f64,
    clouds: &CloudModel,
) -> Result<&'a Chip, String> {
    chip(cache, item, lon, lat, clouds)
}
