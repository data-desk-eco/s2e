//! stac search — 1:1 port of lib/stac.js. two source profiles (aws element84 cog
//! hrefs; cdse copernicus eopf s3://eodata jp2). blocking http (ureq) + serde_json
//! so the fan-out can be plain rayon threads, no async runtime.

use std::time::Duration;

use s2_flares_core::epsg_from_mgrs;
use serde_json::Value;

#[derive(Clone, Debug)]
pub struct Bands {
    pub b12: Option<String>,
    pub b11: Option<String>,
    pub b8a: Option<String>,
    pub scl: Option<String>,
}

#[derive(Clone, Debug)]
#[allow(dead_code)] // cloud_cover/bbox carried for parity; the whole-tile bbox feeds the scene-store cache
pub struct Item {
    pub id: String,
    pub date: String,
    pub cloud_cover: Option<f64>,
    pub mgrs: String,
    pub epsg: i32,
    pub bbox: [f64; 4],
    pub sun_elevation: Option<f64>,
    pub sun_azimuth: Option<f64>,
    pub bands: Bands,
}

fn api(source: &str) -> &'static str {
    match source {
        "cdse" => "https://stac.dataspace.copernicus.eu/v1",
        _ => "https://earth-search.aws.element84.com/v1",
    }
}

fn href(it: &Value, key: &str) -> Option<String> {
    it["assets"][key]["href"].as_str().map(String::from)
}

fn bands_of(it: &Value, source: &str) -> Bands {
    if source == "cdse" {
        Bands { b12: href(it, "B12_20m"), b11: href(it, "B11_20m"), b8a: href(it, "B8A_20m"), scl: href(it, "SCL_20m") }
    } else {
        Bands { b12: href(it, "swir22"), b11: href(it, "swir16"), b8a: href(it, "nir08"), scl: href(it, "scl") }
    }
}

fn epsg_of(it: &Value, source: &str) -> i32 {
    if source == "cdse" {
        epsg_from_mgrs(it["properties"]["grid:code"].as_str().unwrap_or(""))
    } else {
        it["properties"]["proj:epsg"].as_i64().unwrap_or(0) as i32
    }
}

const STAC_ATTEMPTS: usize = 7;

fn retryable(e: &ureq::Error) -> bool {
    match e {
        ureq::Error::Transport(_) => true,
        ureq::Error::Status(code, _) => *code == 408 || *code == 429 || *code >= 500,
    }
}

/// POST one STAC page, retrying transient transport/HTTP failures and malformed
/// responses. CDSE intermittently returns 500/504 under bulk load; failing an AOI
/// search silently omits that site, so retry each pagination request in place rather
/// than forcing a costly second run over the entire AOI catalogue.
fn post_page(url: &str, body: &Value) -> Result<Value, String> {
    for attempt in 1..=STAC_ATTEMPTS {
        let result = ureq::post(url)
            .set("Content-Type", "application/json")
            .send_json(body.clone());
        let err = match result {
            Ok(resp) => match resp.into_json() {
                Ok(data) => return Ok(data),
                Err(e) => format!("stac json: {e}"),
            },
            Err(e) if retryable(&e) => format!("stac http: {e}"),
            Err(e) => return Err(format!("stac http: {e}")),
        };
        if attempt == STAC_ATTEMPTS { return Err(format!("{err} after {STAC_ATTEMPTS} attempts")); }
        let delay = 1u64 << (attempt - 1).min(4); // 1, 2, 4, 8, then 16 s
        eprintln!("  stac transient failure; retry {}/{} in {delay}s: {err}", attempt + 1, STAC_ATTEMPTS);
        std::thread::sleep(Duration::from_secs(delay));
    }
    unreachable!()
}

/// search a date window over a bbox, dedup by mgrs tile + date keeping lowest
/// cloud cover, return normalised items (cloud cover ≤ max_cloud_cover).
pub fn search(bbox: [f64; 4], start: &str, end: &str, max_cloud_cover: f64, source: &str) -> Result<Vec<Item>, String> {
    let base = api(source);
    let payload = serde_json::json!({
        "collections": ["sentinel-2-l2a"],
        "bbox": bbox,
        "datetime": format!("{start}T00:00:00Z/{end}T23:59:59Z"),
        "limit": 100,
    });

    let mut features: Vec<Value> = Vec::new();
    let mut url = format!("{base}/search");
    let mut body = payload;
    loop {
        let data = post_page(&url, &body)?;
        if let Some(arr) = data["features"].as_array() { features.extend(arr.iter().cloned()); }
        // follow the rel:next link (post body) if present.
        let next = data["links"].as_array().and_then(|ls| ls.iter().find(|l| l["rel"] == "next").cloned());
        match next.and_then(|l| Some((l["href"].as_str()?.to_string(), l.get("body")?.clone()))) {
            Some((h, b)) => { url = h; body = b; }
            None => break,
        }
    }

    // dedup by tile+date, keep lowest cloud.
    let mut best: std::collections::HashMap<String, (Value, f64)> = std::collections::HashMap::new();
    for it in features {
        let p = &it["properties"];
        let dt = p["datetime"].as_str().unwrap_or("").get(..10).unwrap_or("").to_string();
        let cloud = p["eo:cloud_cover"].as_f64().unwrap_or(100.0);
        let tile = p["grid:code"].as_str().or_else(|| p["s2:mgrs_tile"].as_str())
            .or_else(|| it["id"].as_str()).unwrap_or("").to_string();
        let key = format!("{tile}_{dt}");
        match best.get(&key) {
            Some((_, c)) if *c <= cloud => {}
            _ => { best.insert(key, (it, cloud)); }
        }
    }

    let mut out = Vec::new();
    for (it, _) in best.into_values() {
        let p = &it["properties"];
        let cloud = p["eo:cloud_cover"].as_f64();
        if cloud.unwrap_or(100.0) > max_cloud_cover { continue; }
        out.push(Item {
            id: it["id"].as_str().unwrap_or("").to_string(),
            date: p["datetime"].as_str().unwrap_or("").get(..10).unwrap_or("").to_string(),
            cloud_cover: cloud,
            mgrs: p["grid:code"].as_str().unwrap_or("").replace("MGRS-", ""),
            epsg: epsg_of(&it, source),
            bbox: {
                let b = it["bbox"].as_array().map(|a| a.iter().filter_map(|v| v.as_f64()).collect::<Vec<_>>()).unwrap_or_default();
                [b.first().copied().unwrap_or(0.0), b.get(1).copied().unwrap_or(0.0),
                 b.get(2).copied().unwrap_or(0.0), b.get(3).copied().unwrap_or(0.0)]
            },
            sun_elevation: p["view:sun_elevation"].as_f64(),
            sun_azimuth: p["view:sun_azimuth"].as_f64(),
            bands: bands_of(&it, source),
        });
    }
    Ok(out)
}
