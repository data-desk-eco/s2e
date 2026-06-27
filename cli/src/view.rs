//! the derived cluster *view* — never the archive. produced by a separate
//! `s2-flares cluster` run over the detection archive (or a fresh detect), one row
//! per cluster + a nested `detections` list column. the web map column-projects to
//! read scalar columns only (cheap pins) and reclusters raw detections via wasm for
//! custom windows; a journalist gets the rich geojson.
//!
//! duckdb owns the parquet+s3 i/o (the stated analytics/archive layer); rust core
//! owns the clustering. the seam is a flat csv handoff — no native parquet deps.

use std::process::Command;
use s2_flares_core::{Cluster, Detection};

fn tmp(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("s2flares-{}-{name}", std::process::id()))
}

fn duckdb(sql: &str) -> Result<(), String> {
    let st = Command::new("duckdb").arg("-c").arg(sql).status().map_err(|e| format!("duckdb spawn: {e}"))?;
    if st.success() { Ok(()) } else { Err("duckdb exited non-zero".into()) }
}

fn opt_f64(s: &str) -> Option<f64> {
    match s.trim() {
        "" => None,
        "Infinity" | "inf" => Some(f64::INFINITY),
        "-Infinity" | "-inf" => Some(f64::NEG_INFINITY),
        v => v.parse().ok(),
    }
}

/// read detections from the archive (any duckdb-readable glob: local parquet/csv or
/// s3://…/flares/preset=…/**/*.parquet), optionally clipped to a bbox + date window.
pub fn read_archive(archive: &str, bbox: Option<[f64; 4]>, start: &str, end: &str) -> Result<Vec<Detection>, String> {
    let out = tmp("dets.csv");
    let out_s = out.to_string_lossy();
    let mut wheres = vec![format!("date >= '{start}' AND date <= '{end}'")];
    if let Some([w, s, e, n]) = bbox {
        wheres.push(format!("lon >= {w} AND lon <= {e} AND lat >= {s} AND lat <= {n}"));
    }
    let sql = format!(
        "INSTALL httpfs; LOAD httpfs; \
         COPY (SELECT lon, lat, date, max_b12, max_b11, b12_b11_ratio, pixels, sun_elevation, sun_azimuth, glint_score \
         FROM read_parquet('{archive}', union_by_name=true) WHERE {}) \
         TO '{out_s}' (FORMAT CSV, HEADER)",
        wheres.join(" AND ")
    );
    duckdb(&sql)?;
    let text = std::fs::read_to_string(&out).map_err(|e| format!("read dets: {e}"))?;
    let _ = std::fs::remove_file(&out);

    let mut dets = Vec::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 10 { continue; }
        dets.push(Detection {
            lon: f[0].parse().unwrap_or(0.0),
            lat: f[1].parse().unwrap_or(0.0),
            date: f[2].to_string(),
            max_b12: f[3].parse().unwrap_or(0.0),
            peak_b11: opt_f64(f[4]),
            b12_b11_ratio: opt_f64(f[5]),
            pixels: f[6].parse().unwrap_or(0),
            sun_elevation: opt_f64(f[7]),
            sun_azimuth: opt_f64(f[8]),
            glint_score: opt_f64(f[9]),
            ..Default::default()
        });
    }
    Ok(dets)
}

fn fmt(x: f64) -> String {
    if x.is_infinite() { if x < 0.0 { "-Infinity".into() } else { "Infinity".into() } } else { format!("{x}") }
}
fn fo(x: Option<f64>) -> String { x.map(fmt).unwrap_or_default() }
fn fb(x: Option<bool>) -> String { x.map(|b| b.to_string()).unwrap_or_default() }

const MEMBER_HEADER: &str = "cluster_id,cluster_lon,cluster_lat,cluster_max_b12,cluster_avg_b12,detection_count,date_count,first_date,last_date,persistence,seasonal,median_b12_b11_ratio,min_sun_elevation,likely_glint,ratio_score,persistence_score,glint_penalty,total_score,max_ratio,min_glint,glint_suspect,date,m_max_b12,peak_b11,pixels,sun_elevation,sun_azimuth,m_lon,m_lat";

// one flat row per (cluster, member detection) — duckdb renests the member fields.
fn member_rows(clusters: &[Cluster]) -> String {
    let mut lines = vec![MEMBER_HEADER.to_string()];
    for c in clusters {
        let head = [
            c.id.clone(), fmt(c.lon), fmt(c.lat), fmt(c.max_b12), fmt(c.avg_b12),
            c.detection_count.to_string(), c.date_count.to_string(), c.first_date.clone(), c.last_date.clone(),
            fo(c.persistence), c.seasonal.to_string(), fo(c.median_b12_b11_ratio), fo(c.min_sun_elevation),
            fb(c.likely_glint), fmt(c.ratio_score), fmt(c.persistence_score), fmt(c.glint_penalty),
            fmt(c.total_score), fo(c.max_ratio), fo(c.min_glint), c.glint_suspect.to_string(),
        ].join(",");
        for d in &c.detections {
            lines.push(format!("{head},{},{},{},{},{},{},{},{}",
                d.date, fmt(d.max_b12), fo(d.peak_b11), d.pixels, fo(d.sun_elevation), fo(d.sun_azimuth), fmt(d.lon), fmt(d.lat)));
        }
    }
    lines.join("\n") + "\n"
}

/// write the cluster view. `.geojson` → rich FeatureCollection (rust). otherwise
/// (a local `.parquet` or an `s3://…/clusters/…` path) → nested parquet via duckdb.
pub fn write_view(clusters: &[Cluster], out: &str) -> Result<(), String> {
    if out.ends_with(".geojson") || out.ends_with(".json") {
        return write_geojson(clusters, out);
    }
    let members = tmp("members.csv");
    std::fs::write(&members, member_rows(clusters)).map_err(|e| format!("write members: {e}"))?;
    let m = members.to_string_lossy();
    // explicit column types → the view's parquet schema is invariant, regardless of
    // whether a run's values happen to be integer-valued (else duckdb infers bigint
    // for one file, double for the next, and a union over the prefix conflicts).
    let cols = "{'cluster_id':'VARCHAR','cluster_lon':'DOUBLE','cluster_lat':'DOUBLE',\
        'cluster_max_b12':'DOUBLE','cluster_avg_b12':'DOUBLE','detection_count':'INTEGER',\
        'date_count':'INTEGER','first_date':'DATE','last_date':'DATE','persistence':'DOUBLE',\
        'seasonal':'BOOLEAN','median_b12_b11_ratio':'DOUBLE','min_sun_elevation':'DOUBLE',\
        'likely_glint':'BOOLEAN','ratio_score':'DOUBLE','persistence_score':'DOUBLE',\
        'glint_penalty':'DOUBLE','total_score':'DOUBLE','max_ratio':'DOUBLE','min_glint':'DOUBLE',\
        'glint_suspect':'BOOLEAN','date':'DATE','m_max_b12':'DOUBLE','peak_b11':'DOUBLE',\
        'pixels':'INTEGER','sun_elevation':'DOUBLE','sun_azimuth':'DOUBLE','m_lon':'DOUBLE','m_lat':'DOUBLE'}";
    // group members back into one row per cluster with a nested `detections` list.
    let sql = format!(
        "INSTALL httpfs; LOAD httpfs; \
         COPY (SELECT cluster_id AS id, \
           any_value(cluster_lon) AS lon, any_value(cluster_lat) AS lat, \
           any_value(cluster_max_b12) AS max_b12, any_value(cluster_avg_b12) AS avg_b12, \
           any_value(detection_count) AS detection_count, any_value(date_count) AS date_count, \
           any_value(first_date) AS first_date, any_value(last_date) AS last_date, \
           any_value(persistence) AS persistence, any_value(seasonal) AS seasonal, \
           any_value(median_b12_b11_ratio) AS median_b12_b11_ratio, any_value(min_sun_elevation) AS min_sun_elevation, \
           any_value(likely_glint) AS likely_glint, any_value(ratio_score) AS ratio_score, \
           any_value(persistence_score) AS persistence_score, any_value(glint_penalty) AS glint_penalty, \
           any_value(total_score) AS total_score, any_value(max_ratio) AS max_ratio, \
           any_value(min_glint) AS min_glint, any_value(glint_suspect) AS glint_suspect, \
           list(struct_pack(date := date, max_b12 := m_max_b12, peak_b11 := peak_b11, pixels := pixels, \
             sun_elevation := sun_elevation, sun_azimuth := sun_azimuth, lon := m_lon, lat := m_lat) ORDER BY date) AS detections \
         FROM read_csv('{m}', header=true, nullstr='', columns={cols}) GROUP BY cluster_id) \
         TO '{out}' (FORMAT PARQUET, COMPRESSION ZSTD)"
    );
    let r = duckdb(&sql);
    let _ = std::fs::remove_file(&members);
    r
}

/// the cluster view as a geojson FeatureCollection string (rich: scalar props +
/// the nested `detections` array). used for file export and stdout.
pub fn geojson(clusters: &[Cluster]) -> String {
    let features: Vec<serde_json::Value> = clusters.iter().map(|c| {
        let props = serde_json::to_value(c).unwrap_or(serde_json::Value::Null);
        serde_json::json!({
            "type": "Feature",
            "geometry": { "type": "Point", "coordinates": [c.lon, c.lat] },
            "properties": props,
        })
    }).collect();
    serde_json::to_string(&serde_json::json!({ "type": "FeatureCollection", "features": features })).unwrap_or_default()
}

fn write_geojson(clusters: &[Cluster], out: &str) -> Result<(), String> {
    std::fs::write(out, geojson(clusters)).map_err(|e| format!("write geojson: {e}"))
}
