//! Publish canonical GeoJSON records unchanged, then optionally rebuild disposable
//! Parquet views. DuckDB remains the columnar engine; Rust owns the operation.

use crate::view;
use std::fs;
use std::path::Path;
use std::process::Command;

fn join(root: &str, tail: &str) -> String {
    format!(
        "{}/{}",
        root.trim_end_matches('/'),
        tail.trim_start_matches('/')
    )
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), String> {
    if !source.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(source).map_err(|e| format!("read {}: {e}", source.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let from = entry.path();
        let to = destination.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to)?;
        } else if !matches!(
            from.extension().and_then(|x| x.to_str()),
            Some("err" | "part")
        ) {
            fs::create_dir_all(destination).map_err(|e| e.to_string())?;
            fs::copy(&from, &to)
                .map_err(|e| format!("copy {} -> {}: {e}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

fn aws_sync(source: &Path, destination: &str) -> Result<(), String> {
    if !source.exists() {
        return Ok(());
    }
    let mut command = Command::new("aws");
    if let Ok(endpoint) = std::env::var("S2_S3_ENDPOINT") {
        let endpoint = if endpoint.starts_with("http") {
            endpoint
        } else {
            format!("https://{endpoint}")
        };
        command.args(["--endpoint-url", &endpoint]);
    }
    command.args([
        "s3",
        "sync",
        source
            .to_str()
            .ok_or_else(|| "non-utf8 input path".to_string())?,
        destination,
        "--exclude",
        "*.err",
        "--exclude",
        "*.part",
        "--only-show-errors",
    ]);
    for (s2, aws) in [
        ("S2_S3_ACCESS_KEY", "AWS_ACCESS_KEY_ID"),
        ("S2_S3_SECRET_KEY", "AWS_SECRET_ACCESS_KEY"),
        ("S2_S3_REGION", "AWS_DEFAULT_REGION"),
    ] {
        if let Ok(value) = std::env::var(s2) {
            command.env(aws, value);
        }
    }
    let status = command.status().map_err(|e| format!("aws s3 sync: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("aws s3 sync exited non-zero".into())
    }
}

pub fn publish(input: &Path, destination: &str, views: bool) -> Result<(), String> {
    let observations = input.join("observations");
    let assets = input.join("assets");
    if destination.starts_with("s3://") {
        aws_sync(&observations, &join(destination, "observations"))?;
        aws_sync(&assets, &join(destination, "assets"))?;
    } else {
        let destination_path = Path::new(destination);
        if input != destination_path {
            copy_tree(&observations, &destination_path.join("observations"))?;
            copy_tree(&assets, &destination_path.join("assets"))?;
        }
    }
    if views {
        derive_views(destination)?;
    }
    Ok(())
}

fn quote(value: &str) -> String {
    value.replace('\'', "''")
}

fn flare_source(root: &str) -> String {
    let glob = quote(&join(root, "observations/**/flares-*.geojson"));
    format!(
        "WITH records AS (SELECT * FROM read_json('{glob}', columns={{'analysis':'JSON','features':'JSON[]'}})), \
         flat AS (SELECT analysis, unnest(features) AS feature FROM records) \
         SELECT json_extract(feature,'$.geometry.coordinates[0]')::DOUBLE AS lon, \
           json_extract(feature,'$.geometry.coordinates[1]')::DOUBLE AS lat, \
           json_extract_string(analysis,'$.scene.date') AS date, \
           json_extract_string(analysis,'$.scene.mgrs') AS mgrs, \
           json_extract_string(analysis,'$.scene.id') AS scene, \
           json_extract(feature,'$.properties.max_b12')::DOUBLE AS max_b12, \
           json_extract(feature,'$.properties.avg_b12')::DOUBLE AS avg_b12, \
           json_extract(feature,'$.properties.peak_b11')::DOUBLE AS max_b11, \
           json_extract(feature,'$.properties.b12_b11_ratio')::DOUBLE AS b12_b11_ratio, \
           json_extract(feature,'$.properties.peakedness')::DOUBLE AS peakedness, \
           json_extract(feature,'$.properties.pixels')::UINTEGER AS pixels, \
           json_extract(feature,'$.properties.radiance')::DOUBLE AS radiance, \
           json_extract(feature,'$.properties.saturated')::UTINYINT AS saturated, \
           json_extract(feature,'$.properties.sun_elevation')::DOUBLE AS sun_elevation, \
           json_extract(feature,'$.properties.sun_azimuth')::DOUBLE AS sun_azimuth, \
           json_extract(feature,'$.properties.glint_angle')::DOUBLE AS glint_angle, \
           json_extract(feature,'$.properties.glint_score')::DOUBLE AS glint_score, \
           json_extract_string(analysis,'$.method.fingerprint') AS method \
         FROM flat"
    )
}

fn duckdb_lines(sql: &str) -> Result<Vec<String>, String> {
    let output = Command::new("duckdb")
        .args(["-csv", "-noheader", "-c", sql])
        .output()
        .map_err(|e| format!("duckdb spawn: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "duckdb exited non-zero: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

fn local_parent(path: &str) -> Result<(), String> {
    if !path.starts_with("s3://") {
        if let Some(parent) = Path::new(path).parent() {
            fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
    }
    Ok(())
}

pub fn derive_views(root: &str) -> Result<(), String> {
    let prelude = view::s3_prelude();
    let flares = flare_source(root);
    let has = |pattern: &str| -> Result<bool, String> {
        Ok(duckdb_lines(&format!(
            "{prelude}SELECT count(*) FROM glob('{}')",
            quote(pattern)
        ))?
        .first()
        .is_some_and(|count| count.trim_matches('"') != "0"))
    };
    if has(&join(root, "observations/**/flares-*.geojson"))? {
        let tiles = duckdb_lines(&format!(
            "{prelude}SELECT DISTINCT mgrs FROM ({flares}) ORDER BY mgrs"
        ))?;
        for tile in tiles {
            let tile = tile.trim_matches('"');
            let output = join(root, &format!("detections/mgrs={tile}/data.parquet"));
            local_parent(&output)?;
            view::duckdb(&format!(
                "{prelude}COPY (SELECT * EXCLUDE(mgrs) FROM ({flares}) WHERE mgrs='{}' ORDER BY date,scene,lon,lat) \
                 TO '{}' (FORMAT PARQUET, COMPRESSION ZSTD)",
                quote(tile), quote(&output)
            ))?;
        }
    }

    let cloud_glob = quote(&join(root, "observations/**/clouds-*.geojson"));
    let cloud_output = join(root, "clouds/data.parquet");
    if has(&join(root, "observations/**/clouds-*.geojson"))? {
        local_parent(&cloud_output)?;
        view::duckdb(&format!(
            "{prelude}COPY (WITH records AS (SELECT * FROM read_json('{cloud_glob}', columns={{'analysis':'JSON','features':'JSON[]'}})), \
               flat AS (SELECT analysis, unnest(features) feature FROM records) \
             SELECT json_extract(feature,'$.geometry.coordinates[0]')::DOUBLE AS glon, \
               json_extract(feature,'$.geometry.coordinates[1]')::DOUBLE AS glat, \
               json_extract_string(feature,'$.properties.date') AS date, \
               json_extract(feature,'$.properties.cloud_fraction')::DOUBLE AS cloud_frac, \
               json_extract_string(analysis,'$.method.fingerprint') AS method FROM flat) \
             TO '{}' (FORMAT PARQUET, COMPRESSION ZSTD)", quote(&cloud_output)
        ))?;
    }

    let plume_glob = quote(&join(root, "observations/**/plumes-*.geojson"));
    let plume_output = join(root, "plumes/results.parquet");
    if has(&join(root, "observations/**/plumes-*.geojson"))? {
        local_parent(&plume_output)?;
        view::duckdb(&format!(
            "{prelude}COPY (WITH records AS (SELECT * FROM read_json('{plume_glob}', columns={{'analysis':'JSON','features':'JSON[]'}})) \
             SELECT json_extract_string(analysis,'$.target.id') AS target_id, \
               json_extract_string(analysis,'$.target.name') AS target_name, \
               json_extract_string(analysis,'$.scene.id') AS scene, \
               json_extract_string(analysis,'$.scene.date') AS date, \
               json_extract_string(analysis,'$.scene.mgrs') AS mgrs, \
               json_extract_string(analysis,'$.status') AS status, \
               json_extract(analysis,'$.clear_percent')::DOUBLE AS clear_percent, \
               json_extract_string(analysis,'$.background_scene') AS background_scene, \
               json_extract(analysis,'$.wind')::DOUBLE[] AS wind, \
               json_extract(analysis,'$.scene_score')::DOUBLE AS scene_score, \
               json_extract_string(analysis,'$.method.fingerprint') AS method, \
               feature IS NOT NULL AS detected, json_extract_string(feature,'$.properties.id') AS id, \
               json_extract(feature,'$.properties.pixels')::UINTEGER AS pixels, \
               json_extract(feature,'$.properties.flux_rate_kg_h')::DOUBLE AS flux_rate_kg_h, \
               json_extract(feature,'$.properties.flux_rate_std_kg_h')::DOUBLE AS flux_rate_std_kg_h, \
               json_extract(feature,'$.geometry')::VARCHAR AS geometry \
             FROM records LEFT JOIN LATERAL unnest(features) u(feature) ON true) \
             TO '{}' (FORMAT PARQUET, COMPRESSION ZSTD)", quote(&plume_output)
        ))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_work_for_local_and_object_storage() {
        assert_eq!(join("out/", "/observations"), "out/observations");
        assert_eq!(join("s3://bucket", "assets"), "s3://bucket/assets");
    }
}
