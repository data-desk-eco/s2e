//! deterministic triage over the published plume detections (UK_REVIEW.md's
//! review layer): duckdb flattens them to scalars, pure s2e_core::triage scores
//! them, and the ranked csv feeds valid-plumes.txt curation. the optional
//! --lines geojson (osm roads/boundaries/waterways) enables the collinearity
//! check against each record's probability asset.

use crate::{read, view};
use s2e_core::review::{dist_m, triage, PlumeCandidate};

const LINE_BUFFER_M: f64 = 15.0; // ~1 px at 10 m

pub fn run(root: &str, lines: Option<&str>, out: Option<&str>) -> Result<(), String> {
    let base = root.trim_end_matches('/');
    let tmp = view::tmp("review.csv");
    let tmp_s = tmp.to_string_lossy();
    // one flat row per plume; the mask envelope corners are extracted in sql so
    // the csv stays quote-free. the observations join is where the facility
    // position and the scene's clear fraction live now — the detection row
    // carries the plume, the observation row carries the look that found it.
    view::duckdb(&format!(
        "{p}COPY (SELECT d.site_id, d.date, str_split(d.id,':')[-1] AS plume_id, d.scene, \
           d.lon, d.lat, o.lon AS target_lon, o.lat AS target_lat, \
           d.wind[1] AS wu, d.wind[2] AS wv, d.sun_elevation, \
           (1 - o.cloud) * 100 AS clear_percent, d.rate_kg_h, d.max_probability, \
           json_extract(d.geometry,'$.coordinates[0][0][0]')::DOUBLE AS gw, \
           json_extract(d.geometry,'$.coordinates[0][0][1]')::DOUBLE AS gs, \
           json_extract(d.geometry,'$.coordinates[0][2][0]')::DOUBLE AS ge, \
           json_extract(d.geometry,'$.coordinates[0][2][1]')::DOUBLE AS gn, \
           d.probability_asset, d.background_scene \
         FROM read_parquet('{base}/data-desk/detections/**/data.parquet') d \
         LEFT JOIN read_parquet('{base}/data-desk/observations/**/data.parquet') o \
           ON o.provider=d.provider AND o.kind=d.kind AND o.site_id=d.site_id AND o.date=d.date \
         WHERE d.kind='plume' AND d.provider='data-desk' AND d.status='ok') \
         TO '{tmp_s}' (FORMAT CSV, HEADER)",
        p = view::s3_prelude()
    ))?;
    let text = std::fs::read_to_string(&tmp).map_err(|e| format!("read rows: {e}"))?;
    let _ = std::fs::remove_file(&tmp);

    let segments = lines.map(line_segments).transpose()?;
    let mut cands = Vec::new();
    let mut orbits = Vec::new();
    let mut rows = text.lines();
    let cols = view::columns(rows.next().unwrap_or_default());
    for line in rows.filter(|l| !l.is_empty()) {
        let v: Vec<&str> = line.split(',').collect();
        let s = |n: &str| cols.get(n).and_then(|&i| v.get(i)).copied().unwrap_or("");
        let num = |n: &str| s(n).parse::<f64>().ok();
        let (Some(clon), Some(clat)) = (num("lon"), num("lat")) else {
            continue;
        };
        let centre = [clon, clat];
        let target = [
            num("target_lon").unwrap_or(clon),
            num("target_lat").unwrap_or(clat),
        ];
        // mask envelope (ring corners 0 and 2), centre when geometry was null.
        let (gw, gs, ge, gn) = (
            num("gw").unwrap_or(clon),
            num("gs").unwrap_or(clat),
            num("ge").unwrap_or(clon),
            num("gn").unwrap_or(clat),
        );
        let env = [gw.min(ge), gs.min(gn), gw.max(ge), gs.max(gn)];
        // plume onset: the envelope point nearest the facility.
        let origin = [
            target[0].clamp(env[0], env[2]),
            target[1].clamp(env[1], env[3]),
        ];
        let asset = s("probability_asset");
        let collinearity = segments
            .as_deref()
            .filter(|_| !asset.is_empty())
            .and_then(|seg| collinearity(base, asset, env, seg));
        orbits.push(orbit(s("scene")));
        cands.push(PlumeCandidate {
            // the curation key valid-plumes.txt holds, unchanged by the reshape:
            // the site is the target it always was, and the plume component is
            // the last field of the detection id the table now builds.
            key: format!("{}:{}:{}", s("site_id"), s("date"), s("plume_id")),
            target: s("site_id").into(),
            date: s("date").into(),
            centre,
            origin,
            anchor_m: dist_m(target, origin),
            wind: [num("wu").unwrap_or(0.0), num("wv").unwrap_or(0.0)],
            max_p: num("max_probability").unwrap_or(0.0),
            flux_kg_h: num("rate_kg_h").unwrap_or(0.0),
            clear_percent: num("clear_percent").unwrap_or(100.0),
            // null on every plume row until the detections table carries the
            // scene's sun elevation as a plume extension; the term is inert
            // rather than wrong while it is.
            sun_elevation: num("sun_elevation"),
            cross_orbit_bg: {
                let (a, b) = (orbit(s("scene")), orbit(s("background_scene")));
                !a.is_empty() && !b.is_empty() && a != b
            },
            collinearity,
        });
    }

    let verdicts = triage(&cands);
    let mut body = String::from(
        "key,verdict,score,rejects,flags,orbit,max_p,flux_kg_h,anchor_m,collinearity\n",
    );
    for x in &verdicts {
        let c = &cands[x.idx];
        body += &format!(
            "{},{},{:.2},{},{},{},{:.2},{:.0},{:.0},{}\n",
            x.key,
            if x.rejects.is_empty() {
                "review"
            } else {
                "reject"
            },
            x.score,
            x.rejects.join(";"),
            x.flags.join(";"),
            orbits[x.idx],
            c.max_p,
            c.flux_kg_h,
            c.anchor_m,
            c.collinearity
                .map(|v| format!("{v:.2}"))
                .unwrap_or_default()
        );
    }
    match out {
        Some(path) => std::fs::write(path, body).map_err(|e| format!("write {path}: {e}"))?,
        None => print!("{body}"),
    }
    eprintln!(
        "review: {} candidates → {} survivors",
        verdicts.len(),
        verdicts.iter().filter(|x| x.rejects.is_empty()).count()
    );
    Ok(())
}

// relative orbit from the scene id (…_R037_…) — a dual-geometry curation aid.
fn orbit(scene: &str) -> String {
    scene
        .split('_')
        .find(|t| t.len() == 4 && t.starts_with('R') && t[1..].bytes().all(|b| b.is_ascii_digit()))
        .unwrap_or("")
        .to_string()
}

/// every polyline/ring in a geojson file as lon/lat segments.
fn line_segments(path: &str) -> Result<Vec<[f64; 4]>, String> {
    let gj: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}"))?,
    )
    .map_err(|e| format!("parse {path}: {e}"))?;
    fn walk(v: &serde_json::Value, segs: &mut Vec<[f64; 4]>) {
        let Some(a) = v.as_array() else { return };
        let positions: Vec<[f64; 2]> = a
            .iter()
            .filter_map(|p| {
                let p = p.as_array()?;
                Some([p.first()?.as_f64()?, p.get(1)?.as_f64()?])
            })
            .collect();
        if positions.len() == a.len() && !positions.is_empty() {
            for w in positions.windows(2) {
                segs.push([w[0][0], w[0][1], w[1][0], w[1][1]]);
            }
        } else {
            for x in a {
                walk(x, segs);
            }
        }
    }
    let mut segs = Vec::new();
    for f in gj["features"].as_array().into_iter().flatten() {
        walk(&f["geometry"]["coordinates"], &mut segs);
    }
    Ok(segs)
}

fn seg_dist(p: [f64; 2], s: [f64; 4]) -> f64 {
    let (vx, vy) = (s[2] - s[0], s[3] - s[1]);
    let l2 = vx * vx + vy * vy;
    let t = if l2 > 0.0 {
        (((p[0] - s[0]) * vx + (p[1] - s[1]) * vy) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p[0] - s[0] - t * vx).hypot(p[1] - s[1] - t * vy)
}

/// boundary-following score for one component: read the probability asset, take
/// its mask pixels inside the component envelope, and combine the fraction lying
/// within ~1 px of a linear feature with the mask principal axis' alignment to
/// the feature nearest the mask centroid.
fn collinearity(root: &str, asset: &str, env: [f64; 4], segs: &[[f64; 4]]) -> Option<f64> {
    let r = read::open(&format!("{root}/{asset}")).ok()?;
    let epsg = r.ds.spatial_ref().ok()?.auth_code().ok()?;
    let (zone, north) = s2e_core::utm_params(epsg);
    let to_utm = |lon: f64, lat: f64| s2e_core::wgs84_to_utm(lon, lat, zone, north);
    let (ax, ay) = to_utm(env[0], env[1]);
    let (bx, by) = to_utm(env[2], env[3]);
    let (x0, x1) = (ax.min(bx) - 1.0, ax.max(bx) + 1.0);
    let (y0, y1) = (ay.min(by) - 1.0, ay.max(by) + 1.0);
    let probability = read::read_window::<f32>(&r, [0, 0, r.width, r.height]).ok()?;
    let mut pts = Vec::new();
    for (i, &p) in probability.iter().enumerate() {
        if p > s2e_core::plume::DEFAULT_THRESHOLD {
            let x = r.bbox[0] + ((i % r.width) as f64 + 0.5) * r.res_x;
            let y = r.bbox[3] - ((i / r.width) as f64 + 0.5) * r.res_y;
            if x >= x0 && x <= x1 && y >= y0 && y <= y1 {
                pts.push([x, y]);
            }
        }
    }
    if pts.len() < 3 {
        return None;
    }
    let near: Vec<[f64; 4]> = segs
        .iter()
        .map(|s| {
            let (px, py) = to_utm(s[0], s[1]);
            let (qx, qy) = to_utm(s[2], s[3]);
            [px, py, qx, qy]
        })
        .filter(|s| {
            s[0].max(s[2]) >= x0 - 200.0
                && s[0].min(s[2]) <= x1 + 200.0
                && s[1].max(s[3]) >= y0 - 200.0
                && s[1].min(s[3]) <= y1 + 200.0
        })
        .collect();
    if near.is_empty() {
        return Some(0.0);
    }
    let on_line = pts
        .iter()
        .filter(|p| near.iter().any(|s| seg_dist(**p, *s) <= LINE_BUFFER_M))
        .count();
    let frac = on_line as f64 / pts.len() as f64;
    // principal axis (leading eigenvector of the mask covariance)…
    let n = pts.len() as f64;
    let mx = pts.iter().map(|p| p[0]).sum::<f64>() / n;
    let my = pts.iter().map(|p| p[1]).sum::<f64>() / n;
    let (mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0);
    for p in &pts {
        let (dx, dy) = (p[0] - mx, p[1] - my);
        sxx += dx * dx;
        syy += dy * dy;
        sxy += dx * dy;
    }
    let theta = 0.5 * (2.0 * sxy).atan2(sxx - syy);
    // …vs the direction of the segment nearest the centroid.
    let s = near
        .iter()
        .min_by(|a, b| seg_dist([mx, my], **a).total_cmp(&seg_dist([mx, my], **b)))?;
    let (dx, dy) = (s[2] - s[0], s[3] - s[1]);
    let len = dx.hypot(dy);
    let align = if len > 0.0 {
        (theta.cos() * dx / len + theta.sin() * dy / len).abs()
    } else {
        0.0
    };
    Some(frac * align)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orbit_from_scene_id() {
        assert_eq!(
            orbit("S2B_MSIL1C_20260423T112109_N0512_R037_T30UWD"),
            "R037"
        );
        assert_eq!(orbit("weird"), "");
    }

    #[test]
    fn segments_from_any_geometry() {
        let f = view::tmp("lines.geojson");
        std::fs::write(&f, r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","geometry":{"type":"LineString","coordinates":[[0,0],[1,0],[1,1]]}},
            {"type":"Feature","geometry":{"type":"MultiLineString","coordinates":[[[2,2],[3,3]]]}},
            {"type":"Feature","geometry":{"type":"Polygon","coordinates":[[[5,5],[6,5],[6,6],[5,5]]]}}]}"#).unwrap();
        let segs = line_segments(&f.to_string_lossy()).unwrap();
        let _ = std::fs::remove_file(&f);
        assert_eq!(segs.len(), 2 + 1 + 3);
    }

    #[test]
    fn point_to_segment() {
        assert_eq!(seg_dist([0.0, 1.0], [-1.0, 0.0, 1.0, 0.0]), 1.0);
        assert_eq!(seg_dist([2.0, 0.0], [-1.0, 0.0, 1.0, 0.0]), 1.0); // clamped end
        assert_eq!(seg_dist([1.0, 1.0], [0.0, 0.0, 0.0, 0.0]), 2f64.sqrt()); // degenerate
    }
}
