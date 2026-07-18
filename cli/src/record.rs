//! Canonical detector output: one immutable GeoJSON FeatureCollection for one
//! detector, target area, Sentinel-2 scene and methodology fingerprint.

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

// v2: scene-level constants (sun geometry, glint, date) live once on the
// analysis; features carry only per-detection values.
pub const SCHEMA: &str = "s2-emissions/analysis/v2";

pub fn bbox_geometry([w, s, e, n]: [f64; 4]) -> Value {
    json!({
        "type": "Polygon",
        "coordinates": [[[w,s],[e,s],[e,n],[w,n],[w,s]]]
    })
}

pub fn fingerprint(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).expect("JSON values serialize");
    let digest = Sha256::digest(bytes);
    format!("{:x}", digest)[..16].to_string()
}

pub fn safe_id(id: &str) -> String {
    let clean: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if clean.is_empty() {
        "area".into()
    } else {
        clean
    }
}

pub fn area_key(id: &str, geometry: &Value) -> String {
    format!("{}-{}", safe_id(id), &fingerprint(geometry)[..8])
}

pub fn analysis_path(
    root: &Path,
    area: &str,
    scene: &str,
    detector: &str,
    method: &str,
) -> PathBuf {
    root.join("observations")
        .join(area)
        .join(safe_id(scene))
        .join(format!("{}-{}.geojson", safe_id(detector), safe_id(method)))
}

pub fn error_path(root: &Path, area: &str, scene: &str, detector: &str) -> PathBuf {
    root.join("observations")
        .join(area)
        .join(safe_id(scene))
        .join(format!("{}.err", safe_id(detector)))
}

pub fn feature(geometry: Value, properties: Value) -> Value {
    json!({
        "type": "Feature",
        "geometry": geometry,
        "properties": properties
    })
}

/// RFC 7946 permits foreign members on a FeatureCollection. `analysis` is the
/// provenance/status envelope; `features` is always the zero-to-many result set.
pub fn collection(analysis: Value, features: Vec<Value>) -> Value {
    json!({
        "type": "FeatureCollection",
        "schema": SCHEMA,
        "analysis": analysis,
        "features": features
    })
}

pub fn target(id: &str, name: &str, geometry: &Value, properties: &Map<String, Value>) -> Value {
    json!({
        "id": id,
        "name": name,
        "geometry": geometry,
        "properties": properties
    })
}

pub fn scene(item: &crate::stac::Item, source: &str) -> Value {
    json!({
        "id": item.id,
        "date": item.date,
        "datetime": item.datetime,
        "mgrs": item.mgrs,
        "satellite": item.id.get(..3).unwrap_or(""),
        "level": item.level,
        "source": source,
        "sun_elevation": item.sun_elevation,
        "sun_azimuth": item.sun_azimuth,
        "epsg": item.epsg,
        "footprint": bbox_geometry(item.bbox)
    })
}

pub fn is_complete(path: &Path, detector: &str, scene: &str, method: &str) -> bool {
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return false;
    };
    let identity_matches = value["type"] == "FeatureCollection"
        && value["schema"] == SCHEMA
        && value["analysis"]["detector"] == detector
        && value["analysis"]["scene"]["id"] == scene
        && value["analysis"]["method"]["fingerprint"] == method
        && value["features"].is_array();
    if !identity_matches {
        return false;
    }
    if let Some(cloud) = value["analysis"]["cloud_analysis"].as_str() {
        if !path.with_file_name(cloud).is_file() {
            return false;
        }
    }
    if let Some(asset) = value["analysis"]["assets"]["probability"].as_str() {
        let Some(root) = path.ancestors().nth(4) else {
            return false;
        };
        if !root.join(asset).is_file() {
            return false;
        }
    }
    true
}

pub fn atomic_write(path: &Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let name = path
        .file_name()
        .ok_or_else(|| "output has no filename".to_string())?;
    let part = path.with_file_name(format!(".{}.part", name.to_string_lossy()));
    let mut body = serde_json::to_vec(value).map_err(|e| format!("serialize record: {e}"))?;
    body.push(b'\n');
    fs::write(&part, body).map_err(|e| format!("write {}: {e}", part.display()))?;
    fs::rename(&part, path)
        .map_err(|e| format!("commit {} -> {}: {e}", part.display(), path.display()))
}

pub fn persist_error(path: &Path, message: &str) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, format!("{message}\n"));
}

pub fn clear_error(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("remove {}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_is_geojson_and_identity_is_stable() {
        let geometry = bbox_geometry([-1.0, 50.0, 1.0, 52.0]);
        let c = collection(json!({"detector":"flares"}), vec![]);
        assert_eq!(c["type"], "FeatureCollection");
        assert!(c["features"].as_array().unwrap().is_empty());
        assert_eq!(area_key("test", &geometry), area_key("test", &geometry));
    }
}
