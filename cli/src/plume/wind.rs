//! Retry-safe GEOS-FP acquisition, bounded caching and NetCDF sampling.

use chrono::{DateTime, Datelike, Timelike, Utc};
use gdal::Dataset;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

const CACHE_FILES: usize = 40;

fn file(dir: &Path, date: DateTime<Utc>) -> PathBuf {
    dir.join("wind").join(format!(
        "GEOS.fp.asm.tavg1_2d_slv_Nx.{:04}{:02}{:02}_{:02}30.V01.nc4",
        date.year(),
        date.month(),
        date.day(),
        date.hour()
    ))
}

fn prune(dir: &Path, current: &Path) {
    prune_to(dir, current, CACHE_FILES);
}

fn prune_to(dir: &Path, current: &Path, limit: usize) {
    let Ok(entries) = fs::read_dir(dir.join("wind")) else {
        return;
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "nc4"))
        .collect();
    files.sort();
    while files.len() > limit {
        let Some(index) = files.iter().position(|path| path != current) else {
            break;
        };
        let victim = files.remove(index);
        let _ = fs::remove_file(victim);
    }
}

pub fn sample(dir: &Path, date: DateTime<Utc>, lon: f64, lat: f64) -> Result<[f32; 2], String> {
    let path = file(dir, date);
    if !path.exists() {
        fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
        let url = format!(
            "https://portal.nccs.nasa.gov/datashare/gmao/geos-fp/das/Y{:04}/M{:02}/D{:02}/{}",
            date.year(),
            date.month(),
            date.day(),
            path.file_name().unwrap().to_string_lossy()
        );
        eprintln!("wind: {}", path.display());
        let part = path.with_extension("part");
        let mut last_error = String::new();
        for attempt in 1..=4 {
            let result = (|| -> Result<(), String> {
                let response = ureq::get(&url)
                    .call()
                    .map_err(|e| format!("request: {e}"))?;
                let expected = response
                    .header("Content-Length")
                    .and_then(|x| x.parse::<u64>().ok());
                let mut input = response.into_reader();
                let mut output = File::create(&part).map_err(|e| e.to_string())?;
                let written =
                    std::io::copy(&mut input, &mut output).map_err(|e| format!("transfer: {e}"))?;
                output.flush().map_err(|e| e.to_string())?;
                if expected.is_some_and(|n| n != written) {
                    return Err(format!(
                        "short transfer: {written}/{} bytes",
                        expected.unwrap()
                    ));
                }
                fs::rename(&part, &path).map_err(|e| e.to_string())
            })();
            match result {
                Ok(()) => {
                    last_error.clear();
                    break;
                }
                Err(e) => {
                    last_error = e;
                    let _ = fs::remove_file(&part);
                    if attempt < 4 {
                        let delay = 1u64 << (attempt - 1);
                        eprintln!(
                            "    wind transfer retry {}/4 in {delay}s: {last_error}",
                            attempt + 1
                        );
                        std::thread::sleep(std::time::Duration::from_secs(delay));
                    }
                }
            }
        }
        if !last_error.is_empty() {
            return Err(format!("wind {url}: {last_error} after 4 attempts"));
        }
    }
    let sampled = [
        sample_netcdf(&path, "U10M", lon, lat)?,
        sample_netcdf(&path, "V10M", lon, lat)?,
    ];
    prune(dir, &path);
    Ok(sampled)
}

fn sample_netcdf(path: &Path, variable: &str, lon: f64, lat: f64) -> Result<f32, String> {
    let name = format!("NETCDF:\"{}\":{variable}", path.display());
    let ds = Dataset::open(&name).map_err(|e| format!("open {name}: {e}"))?;
    let gt = ds
        .geo_transform()
        .map_err(|e| format!("wind geotransform: {e}"))?;
    let (width, height) = ds.raster_size();
    let px = (lon - (gt[0] + gt[1] * 0.5)) / gt[1];
    let py = (lat - (gt[3] + gt[5] * 0.5)) / gt[5];
    let x0 = px.floor().clamp(0.0, width.saturating_sub(2) as f64) as isize;
    let y0 = py.floor().clamp(0.0, height.saturating_sub(2) as f64) as isize;
    let a = ds
        .rasterband(1)
        .map_err(|e| e.to_string())?
        .read_as::<f32>((x0, y0), (2, 2), (2, 2), None)
        .map_err(|e| e.to_string())?;
    let (wx, wy) = (
        (px - x0 as f64).clamp(0.0, 1.0) as f32,
        (py - y0 as f64).clamp(0.0, 1.0) as f32,
    );
    let d = a.data();
    let value = (d[0] * (1.0 - wx) + d[1] * wx) * (1.0 - wy) + (d[2] * (1.0 - wx) + d[3] * wx) * wy;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(format!("wind is NaN at {lon},{lat}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_is_bounded_without_evicting_the_active_file() {
        let root = std::env::temp_dir().join(format!("s2e-wind-cache-{}", std::process::id()));
        let wind = root.join("wind");
        fs::create_dir_all(&wind).unwrap();
        let current = wind.join("a.nc4");
        for name in ["a.nc4", "b.nc4", "c.nc4", "d.nc4"] {
            fs::write(wind.join(name), name).unwrap();
        }
        prune_to(&root, &current, 2);
        let mut remaining: Vec<_> = fs::read_dir(&wind)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        remaining.sort();
        assert_eq!(remaining, ["a.nc4", "d.nc4"]);
        fs::remove_dir_all(root).unwrap();
    }
}
