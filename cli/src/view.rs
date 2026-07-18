//! the derived cluster *view* — never the archive. produced by a separate
//! `s2-flares cluster` run over the detection archive (or a fresh detect), one row
//! per cluster + a nested `detections` list column. the web map column-projects to
//! read scalar columns only (cheap pins) and reclusters raw detections via wasm for
//! custom windows; a journalist gets the rich geojson.
//!
//! duckdb owns the parquet+s3 i/o (the stated analytics/archive layer); rust core
//! owns the clustering. the seam is a flat csv handoff — no native parquet deps.

use s2_flares_core::{Cluster, Detection};
use std::process::{Command, Stdio};

pub(crate) fn tmp(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("s2flares-{}-{name}", std::process::id()))
}

pub(crate) fn duckdb(sql: &str) -> Result<(), String> {
    let st = Command::new("duckdb")
        .arg("-c")
        .arg(sql)
        .status()
        .map_err(|e| format!("duckdb spawn: {e}"))?;
    if st.success() {
        Ok(())
    } else {
        Err("duckdb exited non-zero".into())
    }
}

// duckdb s3 prelude. with `S2_S3_ENDPOINT` set (the box exports it for CloudFerro)
// we configure a path-style endpoint + creds; otherwise bare httpfs leans on the
// aws default credential chain (local/AWS reads). prepended to every s3-touching sql.
pub(crate) fn s3_prelude() -> String {
    let mut p = String::from("INSTALL httpfs; LOAD httpfs; ");
    if let Ok(ep) = std::env::var("S2_S3_ENDPOINT") {
        let g = |k| std::env::var(k).unwrap_or_default();
        // duckdb's archive creds are kept SEPARATE from AWS_* so the same process can
        // also drive gdal /vsis3 against eodata (AWS_*) during the coverage scan: the
        // duckdb (project-bucket) creds come from S2_S3_* first, falling back to AWS_*.
        let key = |s2, aws| {
            let v = g(s2);
            if v.is_empty() {
                g(aws)
            } else {
                v
            }
        };
        p += &format!(
            "SET s3_endpoint='{ep}'; SET s3_region='{}'; SET s3_url_style='path'; \
            SET s3_use_ssl=true; SET s3_access_key_id='{}'; SET s3_secret_access_key='{}'; ",
            g("S2_S3_REGION"),
            key("S2_S3_ACCESS_KEY", "AWS_ACCESS_KEY_ID"),
            key("S2_S3_SECRET_KEY", "AWS_SECRET_ACCESS_KEY")
        );
    }
    p
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
/// s3://…/detections/**/*.parquet), optionally clipped to a bbox + date window.
pub fn read_archive(
    archive: &str,
    bbox: Option<[f64; 4]>,
    start: &str,
    end: &str,
) -> Result<Vec<Detection>, String> {
    let out = tmp("dets.csv");
    let out_s = out.to_string_lossy();
    let mut wheres = vec![format!("date >= '{start}' AND date <= '{end}'")];
    if let Some([w, s, e, n]) = bbox {
        wheres.push(format!(
            "lon >= {w} AND lon <= {e} AND lat >= {s} AND lat <= {n}"
        ));
    }
    // hive_partitioning exposes the detections' `mgrs` path key so each cluster can
    // inherit its anchor's tile → the view partitions by mgrs like the archive.
    // `radiance` is newer than `pixels`/`warm_size` — a pre-fix archive lacks it, so the
    // false-filtered template declares the column and union_by_name backfills NULL (→ 0)
    // for legacy rows instead of duckdb erroring. (legacy `pixels` is still the old
    // flooded count, though — re-detect for a clean volume.)
    let sql = format!(
        "{prelude}\
         COPY (SELECT lon, lat, date, mgrs, max_b12, max_b11, b12_b11_ratio, pixels, sun_elevation, sun_azimuth, glint_score, radiance \
         FROM (SELECT * FROM read_parquet('{archive}', union_by_name=true, hive_partitioning=true) \
               UNION ALL BY NAME SELECT NULL::DOUBLE AS radiance WHERE false) WHERE {wheres}) \
         TO '{out_s}' (FORMAT CSV, HEADER)",
        prelude = s3_prelude(), wheres = wheres.join(" AND ")
    );
    duckdb(&sql)?;
    let text = std::fs::read_to_string(&out).map_err(|e| format!("read dets: {e}"))?;
    let _ = std::fs::remove_file(&out);

    let mut dets = Vec::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 12 {
            continue;
        }
        dets.push(Detection {
            lon: f[0].parse().unwrap_or(0.0),
            lat: f[1].parse().unwrap_or(0.0),
            date: f[2].to_string(),
            mgrs: f[3].to_string(),
            max_b12: f[4].parse().unwrap_or(0.0),
            peak_b11: opt_f64(f[5]),
            b12_b11_ratio: opt_f64(f[6]),
            pixels: f[7].parse().unwrap_or(0),
            sun_elevation: opt_f64(f[8]),
            sun_azimuth: opt_f64(f[9]),
            glint_score: opt_f64(f[10]),
            radiance: f[11].parse().unwrap_or(0.0),
            ..Default::default()
        });
    }
    Ok(dets)
}

/// read the cloud mask (clouds/ glob/parquet: glon,glat,date,cloud_frac) over a date
/// window, restricted to `cells` (grid indices round(lon/GRID_STEP), round(lat/GRID_STEP)):
/// the semi-join runs INSIDE duckdb (tiny csv build side, mask side streams), so the
/// result — which the duckdb cli materialises in full before printing — is
/// O(anchor cells × dates), not O(mask). the unfiltered mask (~25 GB materialised)
/// OOM-killed a 7 GB box even with 16 GB of swap.
pub fn read_clouds(
    glob: &str,
    start: &str,
    end: &str,
    cells: &std::collections::HashSet<(i64, i64)>,
    mut sink: impl FnMut(f64, f64, &str, f64),
) -> Result<(), String> {
    let cf = tmp("cells.csv");
    let cf_s = cf.to_string_lossy();
    let body: String = std::iter::once("i,j".to_string())
        .chain(cells.iter().map(|(i, j)| format!("{i},{j}")))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&cf, body).map_err(|e| format!("write cells: {e}"))?;
    let inv = (1.0 / s2_flares_core::GRID_STEP).round();
    let sql = format!(
        "{prelude}SELECT DISTINCT glon, glat, date, cloud_frac \
         FROM read_parquet('{glob}', union_by_name=true) \
         JOIN read_csv('{cf_s}', header=true, columns={{'i':'BIGINT','j':'BIGINT'}}) \
           ON CAST(round(glon*{inv}) AS BIGINT)=i AND CAST(round(glat*{inv}) AS BIGINT)=j \
         WHERE date >= '{start}' AND date <= '{end}'",
        prelude = s3_prelude()
    );
    let mut child = Command::new("duckdb")
        .args(["-csv", "-noheader", "-c", &sql])
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("duckdb spawn: {e}"))?;
    use std::io::BufRead;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "duckdb stdout unavailable".to_string())?;
    for line in std::io::BufReader::new(stdout)
        .lines()
        .map_while(Result::ok)
    {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 4 {
            continue;
        }
        sink(
            f[0].parse().unwrap_or(0.0),
            f[1].parse().unwrap_or(0.0),
            f[2],
            f[3].parse().unwrap_or(1.0),
        );
    }
    let status = child.wait().map_err(|e| format!("duckdb wait: {e}"))?;
    let _ = std::fs::remove_file(&cf);
    if status.success() {
        Ok(())
    } else {
        Err("duckdb exited non-zero".into())
    }
}

/// distinct mgrs tiles in the archive + each tile's detection bounding box — the
/// per-tile STAC search areas for the coverage scan. reads the hive `mgrs` partition
/// key (the per-tile rollup EXCLUDEs mgrs from the file body, keeps it as the path).
pub fn tile_bboxes(
    archive: &str,
    start: &str,
    end: &str,
) -> Result<Vec<(String, [f64; 4])>, String> {
    let out = tmp("tiles.csv");
    let out_s = out.to_string_lossy();
    let sql = format!(
        "{prelude}COPY (SELECT mgrs, min(lon) AS w, min(lat) AS s, max(lon) AS e, max(lat) AS n \
         FROM read_parquet('{archive}', hive_partitioning=true) \
         WHERE date >= '{start}' AND date <= '{end}' GROUP BY mgrs) TO '{out_s}' (FORMAT CSV, HEADER)",
        prelude = s3_prelude());
    duckdb(&sql)?;
    let text = std::fs::read_to_string(&out).map_err(|e| format!("read tiles: {e}"))?;
    let _ = std::fs::remove_file(&out);
    let mut v = Vec::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 5 || f[0].is_empty() {
            continue;
        }
        v.push((
            f[0].to_string(),
            [
                f[1].parse().unwrap_or(0.0),
                f[2].parse().unwrap_or(0.0),
                f[3].parse().unwrap_or(0.0),
                f[4].parse().unwrap_or(0.0),
            ],
        ));
    }
    Ok(v)
}

fn fmt(x: f64) -> String {
    if x.is_infinite() {
        if x < 0.0 {
            "-Infinity".into()
        } else {
            "Infinity".into()
        }
    } else {
        format!("{x}")
    }
}
fn fo(x: Option<f64>) -> String {
    x.map(fmt).unwrap_or_default()
}
fn fb(x: Option<bool>) -> String {
    x.map(|b| b.to_string()).unwrap_or_default()
}

const MEMBER_HEADER: &str = "cluster_id,mgrs,cluster_lon,cluster_lat,cluster_max_b12,cluster_avg_b12,cluster_radiance,detection_count,date_count,first_date,last_date,persistence,seasonal,median_b12_b11_ratio,min_sun_elevation,likely_glint,ratio_score,persistence_score,glint_penalty,total_score,max_ratio,min_glint,glint_suspect,date,m_max_b12,peak_b11,pixels,radiance,sun_elevation,sun_azimuth,m_lon,m_lat";

// one flat row per (cluster, member detection) — duckdb renests the member fields.
fn member_rows(clusters: &[Cluster]) -> String {
    let mut lines = vec![MEMBER_HEADER.to_string()];
    for c in clusters {
        let head = [
            c.id.clone(),
            c.mgrs.clone(),
            fmt(c.lon),
            fmt(c.lat),
            fmt(c.max_b12),
            fmt(c.avg_b12),
            fmt(c.radiance),
            c.detection_count.to_string(),
            c.date_count.to_string(),
            c.first_date.clone(),
            c.last_date.clone(),
            fo(c.persistence),
            c.seasonal.to_string(),
            fo(c.median_b12_b11_ratio),
            fo(c.min_sun_elevation),
            fb(c.likely_glint),
            fmt(c.ratio_score),
            fmt(c.persistence_score),
            fmt(c.glint_penalty),
            fmt(c.total_score),
            fo(c.max_ratio),
            fo(c.min_glint),
            c.glint_suspect.to_string(),
        ]
        .join(",");
        for d in &c.detections {
            lines.push(format!(
                "{head},{},{},{},{},{},{},{},{},{}",
                d.date,
                fmt(d.max_b12),
                fo(d.peak_b11),
                d.pixels,
                fmt(d.radiance),
                fo(d.sun_elevation),
                fo(d.sun_azimuth),
                fmt(d.lon),
                fmt(d.lat)
            ));
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
    let cols = "{'cluster_id':'VARCHAR','mgrs':'VARCHAR','cluster_lon':'DOUBLE','cluster_lat':'DOUBLE',\
        'cluster_max_b12':'DOUBLE','cluster_avg_b12':'DOUBLE','cluster_radiance':'DOUBLE','detection_count':'INTEGER',\
        'date_count':'INTEGER','first_date':'DATE','last_date':'DATE','persistence':'DOUBLE',\
        'seasonal':'BOOLEAN','median_b12_b11_ratio':'DOUBLE','min_sun_elevation':'DOUBLE',\
        'likely_glint':'BOOLEAN','ratio_score':'DOUBLE','persistence_score':'DOUBLE',\
        'glint_penalty':'DOUBLE','total_score':'DOUBLE','max_ratio':'DOUBLE','min_glint':'DOUBLE',\
        'glint_suspect':'BOOLEAN','date':'DATE','m_max_b12':'DOUBLE','peak_b11':'DOUBLE',\
        'pixels':'INTEGER','radiance':'DOUBLE','sun_elevation':'DOUBLE','sun_azimuth':'DOUBLE','m_lon':'DOUBLE','m_lat':'DOUBLE'}";
    // group members back into one row per cluster with a nested `detections` list.
    let grouped = format!(
        "SELECT cluster_id AS id, any_value(mgrs) AS mgrs, \
           any_value(cluster_lon) AS lon, any_value(cluster_lat) AS lat, \
           any_value(cluster_max_b12) AS max_b12, any_value(cluster_avg_b12) AS avg_b12, \
           any_value(cluster_radiance) AS radiance, \
           any_value(detection_count) AS detection_count, any_value(date_count) AS date_count, \
           any_value(first_date) AS first_date, any_value(last_date) AS last_date, \
           any_value(persistence) AS persistence, any_value(seasonal) AS seasonal, \
           any_value(median_b12_b11_ratio) AS median_b12_b11_ratio, any_value(min_sun_elevation) AS min_sun_elevation, \
           any_value(likely_glint) AS likely_glint, any_value(ratio_score) AS ratio_score, \
           any_value(persistence_score) AS persistence_score, any_value(glint_penalty) AS glint_penalty, \
           any_value(total_score) AS total_score, any_value(max_ratio) AS max_ratio, \
           any_value(min_glint) AS min_glint, any_value(glint_suspect) AS glint_suspect, \
           list(struct_pack(date := date, max_b12 := m_max_b12, peak_b11 := peak_b11, pixels := pixels, radiance := radiance, \
             sun_elevation := sun_elevation, sun_azimuth := sun_azimuth, lon := m_lon, lat := m_lat) ORDER BY date) AS detections \
         FROM read_csv('{m}', header=true, nullstr='', columns={cols}) GROUP BY cluster_id");
    let prelude = s3_prelude();
    // a plain `.parquet` → one file, mgrs in the body. a clusters/ dir or s3 prefix →
    // one deterministic `mgrs=<tile>/data.parquet` PER TILE (mgrs in the path, not the
    // body), mirroring detections/ exactly. we loop the tiles ourselves rather than
    // PARTITION_BY because duckdb always indexes the partition filename (`data_0.parquet`
    // — even FILENAME_PATTERN appends the index); a per-tile single-file COPY overwrites
    // its own key idempotently and lands the clean `data.parquet` the readers expect.
    let sql = if out.ends_with(".parquet") {
        format!("{prelude}COPY ({grouped}) TO '{out}' (FORMAT PARQUET, COMPRESSION ZSTD)")
    } else {
        let tiles: std::collections::BTreeSet<&str> =
            clusters.iter().map(|c| c.mgrs.as_str()).collect();
        let base = out.trim_end_matches('/');
        // a single-file COPY won't create the mgrs=…/ dir (PARTITION_BY did); s3 keys need
        // no mkdir, a local path does.
        if !base.starts_with("s3://") {
            for t in &tiles {
                let _ = std::fs::create_dir_all(format!("{base}/mgrs={t}"));
            }
        }
        let copies: String = tiles.iter().map(|t| format!(
            "COPY (SELECT * EXCLUDE(mgrs) FROM v WHERE mgrs='{t}') TO '{base}/mgrs={t}/data.parquet' (FORMAT PARQUET, COMPRESSION ZSTD);"
        )).collect();
        format!("{prelude}CREATE TEMP TABLE v AS {grouped};\n{copies}")
    };
    let r = duckdb(&sql);
    let _ = std::fs::remove_file(&members);
    r
}

/// the cluster view as a geojson FeatureCollection string (rich: scalar props +
/// the nested `detections` array). used for file export and stdout.
pub fn geojson(clusters: &[Cluster]) -> String {
    let features: Vec<serde_json::Value> = clusters
        .iter()
        .map(|c| {
            let props = serde_json::to_value(c).unwrap_or(serde_json::Value::Null);
            serde_json::json!({
                "type": "Feature",
                "geometry": { "type": "Point", "coordinates": [c.lon, c.lat] },
                "properties": props,
            })
        })
        .collect();
    serde_json::to_string(&serde_json::json!({ "type": "FeatureCollection", "features": features }))
        .unwrap_or_default()
}

fn write_geojson(clusters: &[Cluster], out: &str) -> Result<(), String> {
    std::fs::write(out, geojson(clusters)).map_err(|e| format!("write geojson: {e}"))
}
