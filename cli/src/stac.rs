//! stac search — 1:1 port of lib/stac.js. two source profiles (aws element84 cog
//! hrefs; cdse copernicus eopf s3://eodata jp2). blocking http (ureq) + serde_json
//! so the fan-out can be plain rayon threads, no async runtime.

use std::time::Duration;

use s2e_core::epsg_from_mgrs;
use serde_json::Value;

#[derive(Clone, Debug)]
pub struct Bands {
    pub b01: Option<String>,
    pub b02: Option<String>,
    pub b03: Option<String>,
    pub b04: Option<String>,
    pub b05: Option<String>,
    pub b06: Option<String>,
    pub b07: Option<String>,
    pub b08: Option<String>,
    pub b12: Option<String>,
    pub b11: Option<String>,
    pub b8a: Option<String>,
    pub b09: Option<String>,
    pub b10: Option<String>,
    pub scl: Option<String>,
    pub product_metadata: Option<String>,
    pub granule_metadata: Option<String>,
}

#[derive(Clone, Debug)]
#[allow(dead_code)] // cloud_cover/bbox carried for parity; the whole-tile bbox feeds the scene-store cache
pub struct Item {
    pub id: String,
    pub date: String,
    pub datetime: String,
    pub cloud_cover: Option<f64>,
    pub mgrs: String,
    pub epsg: i32,
    pub bbox: [f64; 4],
    pub sun_elevation: Option<f64>,
    pub sun_azimuth: Option<f64>,
    pub bands: Bands,
    /// Sentinel product radiometry: "l1c" (TOA) or "l2a" (surface reflectance).
    pub level: &'static str,
}

fn api(source: &str) -> &'static str {
    if source.starts_with("cdse") {
        "https://stac.dataspace.copernicus.eu/v1"
    } else {
        "https://earth-search.aws.element84.com/v1"
    }
}

fn level(source: &str) -> &'static str {
    if source.ends_with("l1c") {
        "l1c"
    } else {
        "l2a"
    }
}

fn href(it: &Value, key: &str) -> Option<String> {
    it["assets"][key]["href"].as_str().map(String::from)
}

fn aws_l1c_href(it: &Value, key: &str) -> Option<String> {
    href(it, key).map(|url| {
        url.strip_prefix("s3://sentinel-s2-l1c/")
            .map(|path| format!("https://sentinel-s2-l1c.s3.eu-central-1.amazonaws.com/{path}"))
            .unwrap_or(url)
    })
}

fn bands_of(it: &Value, source: &str) -> Bands {
    if source.starts_with("cdse") && level(source) == "l1c" {
        Bands {
            b01: href(it, "B01"),
            b02: href(it, "B02"),
            b03: href(it, "B03"),
            b04: href(it, "B04"),
            b05: href(it, "B05"),
            b06: href(it, "B06"),
            b07: href(it, "B07"),
            b08: href(it, "B08"),
            b8a: href(it, "B8A"),
            b09: href(it, "B09"),
            b10: href(it, "B10"),
            b11: href(it, "B11"),
            b12: href(it, "B12"),
            scl: None,
            product_metadata: href(it, "product_metadata"),
            granule_metadata: href(it, "granule_metadata"),
        }
    } else if source.starts_with("cdse") {
        Bands {
            b01: None,
            b02: None,
            b03: None,
            b04: None,
            b05: None,
            b06: None,
            b07: None,
            b08: None,
            b09: None,
            b10: None,
            b12: href(it, "B12_20m"),
            b11: href(it, "B11_20m"),
            b8a: href(it, "B8A_20m"),
            scl: href(it, "SCL_20m"),
            product_metadata: href(it, "product_metadata"),
            granule_metadata: href(it, "granule_metadata"),
        }
    } else if level(source) == "l1c" {
        Bands {
            b01: aws_l1c_href(it, "coastal"),
            b02: aws_l1c_href(it, "blue"),
            b03: aws_l1c_href(it, "green"),
            b04: aws_l1c_href(it, "red"),
            b05: aws_l1c_href(it, "rededge1"),
            b06: aws_l1c_href(it, "rededge2"),
            b07: aws_l1c_href(it, "rededge3"),
            b08: aws_l1c_href(it, "nir"),
            b8a: aws_l1c_href(it, "nir08"),
            b09: aws_l1c_href(it, "nir09"),
            b10: aws_l1c_href(it, "cirrus"),
            b11: aws_l1c_href(it, "swir16"),
            // Earth Search currently advertises a non-existent
            // `product_metadata.xml` object for this collection.  The tile's
            // `metadata.xml` contains the authoritative L1C quantification and
            // RADIO_ADD_OFFSET values (and is the asset AWS actually serves).
            b12: aws_l1c_href(it, "swir22"),
            scl: None,
            product_metadata: aws_l1c_href(it, "granule_metadata"),
            granule_metadata: aws_l1c_href(it, "granule_metadata"),
        }
    } else {
        Bands {
            b01: None,
            b02: None,
            b03: None,
            b04: None,
            b05: None,
            b06: None,
            b07: None,
            b08: None,
            b09: None,
            b10: None,
            b12: href(it, "swir22"),
            b11: href(it, "swir16"),
            b8a: href(it, "nir08"),
            scl: href(it, "scl"),
            product_metadata: None,
            granule_metadata: None,
        }
    }
}

fn epsg_of(it: &Value, source: &str) -> i32 {
    if source.starts_with("cdse") {
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
        if attempt == STAC_ATTEMPTS {
            return Err(format!("{err} after {STAC_ATTEMPTS} attempts"));
        }
        let delay = 1u64 << (attempt - 1).min(4); // 1, 2, 4, 8, then 16 s
        eprintln!(
            "  stac transient failure; retry {}/{} in {delay}s: {err}",
            attempt + 1,
            STAC_ATTEMPTS
        );
        std::thread::sleep(Duration::from_secs(delay));
    }
    unreachable!()
}

/// search a date window over a bbox, dedup by mgrs tile + date keeping lowest
/// cloud cover, return normalised items (cloud cover ≤ max_cloud_cover).
pub fn search(
    bbox: [f64; 4],
    start: &str,
    end: &str,
    max_cloud_cover: f64,
    source: &str,
) -> Result<Vec<Item>, String> {
    let base = api(source);
    // A GeoJSON Point has a zero-area envelope, which STAC APIs reject.  The
    // plume reader still uses its fixed 2 km chip; this epsilon only makes the
    // catalogue intersection well-defined.
    let mut query_bbox = bbox;
    if query_bbox[0] >= query_bbox[2] {
        query_bbox[0] -= 1e-6;
        query_bbox[2] += 1e-6;
    }
    if query_bbox[1] >= query_bbox[3] {
        query_bbox[1] -= 1e-6;
        query_bbox[3] += 1e-6;
    }
    let payload = serde_json::json!({
        "collections": [format!("sentinel-2-{}", level(source))],
        "bbox": query_bbox,
        "datetime": format!("{start}T00:00:00Z/{end}T23:59:59Z"),
        "limit": 100,
    });

    let mut features: Vec<Value> = Vec::new();
    let mut url = format!("{base}/search");
    let mut body = payload;
    loop {
        let data = post_page(&url, &body)?;
        if let Some(arr) = data["features"].as_array() {
            features.extend(arr.iter().cloned());
        }
        // follow the rel:next link (post body) if present.
        let next = data["links"]
            .as_array()
            .and_then(|ls| ls.iter().find(|l| l["rel"] == "next").cloned());
        match next.and_then(|l| Some((l["href"].as_str()?.to_string(), l.get("body")?.clone()))) {
            Some((h, b)) => {
                url = h;
                body = b;
            }
            None => break,
        }
    }

    // dedup by tile+date, keep lowest cloud.
    let mut best: std::collections::HashMap<String, (Value, f64)> =
        std::collections::HashMap::new();
    for it in features {
        let p = &it["properties"];
        let dt = p["datetime"]
            .as_str()
            .unwrap_or("")
            .get(..10)
            .unwrap_or("")
            .to_string();
        let cloud = p["eo:cloud_cover"].as_f64().unwrap_or(100.0);
        let tile = p["grid:code"]
            .as_str()
            .or_else(|| p["s2:mgrs_tile"].as_str())
            .or_else(|| it["id"].as_str())
            .unwrap_or("")
            .to_string();
        let key = format!("{tile}_{dt}");
        match best.get(&key) {
            Some((_, c)) if *c <= cloud => {}
            _ => {
                best.insert(key, (it, cloud));
            }
        }
    }

    let mut out = Vec::new();
    for (it, _) in best.into_values() {
        let p = &it["properties"];
        let cloud = p["eo:cloud_cover"].as_f64();
        if cloud.unwrap_or(100.0) > max_cloud_cover {
            continue;
        }
        out.push(Item {
            id: it["id"].as_str().unwrap_or("").to_string(),
            date: p["datetime"]
                .as_str()
                .unwrap_or("")
                .get(..10)
                .unwrap_or("")
                .to_string(),
            datetime: p["datetime"].as_str().unwrap_or("").to_string(),
            cloud_cover: cloud,
            mgrs: p["grid:code"]
                .as_str()
                .or_else(|| p["s2:mgrs_tile"].as_str())
                .unwrap_or("")
                .replace("MGRS-", ""),
            epsg: epsg_of(&it, source),
            bbox: {
                let b = it["bbox"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_f64()).collect::<Vec<_>>())
                    .unwrap_or_default();
                [
                    b.first().copied().unwrap_or(0.0),
                    b.get(1).copied().unwrap_or(0.0),
                    b.get(2).copied().unwrap_or(0.0),
                    b.get(3).copied().unwrap_or(0.0),
                ]
            },
            sun_elevation: p["view:sun_elevation"].as_f64(),
            sun_azimuth: p["view:sun_azimuth"].as_f64(),
            bands: bands_of(&it, source),
            level: level(source),
        });
    }
    Ok(out)
}
