//! utm <-> wgs84 (transverse mercator, wgs84 ellipsoid) + mgrs/epsg helpers.
//! 1:1 port of lib/geo.js — zero deps, identical formulae.

const A: f64 = 6378137.0;
const F: f64 = 1.0 / 298.257223563;
const E2: f64 = 2.0 * F - F * F;
const K0: f64 = 0.9996;

fn ep2() -> f64 {
    E2 / (1.0 - E2)
}
fn e1() -> f64 {
    (1.0 - (1.0 - E2).sqrt()) / (1.0 + (1.0 - E2).sqrt())
}

fn m1() -> f64 {
    1.0 - E2 / 4.0 - 3.0 * E2 * E2 / 64.0 - 5.0 * E2 * E2 * E2 / 256.0
}
fn m2() -> f64 {
    3.0 * E2 / 8.0 + 3.0 * E2 * E2 / 32.0 + 45.0 * E2 * E2 * E2 / 1024.0
}
fn m3() -> f64 {
    15.0 * E2 * E2 / 256.0 + 45.0 * E2 * E2 * E2 / 1024.0
}
fn m4() -> f64 {
    35.0 * E2 * E2 * E2 / 3072.0
}

fn central_meridian_rad(zone: i32) -> f64 {
    ((zone - 1) as f64 * 6.0 - 180.0 + 3.0) * core::f64::consts::PI / 180.0
}

fn meridional_arc(phi: f64) -> f64 {
    A * (m1() * phi - m2() * (2.0 * phi).sin() + m3() * (4.0 * phi).sin()
        - m4() * (6.0 * phi).sin())
}

/// [lon, lat] degrees -> [easting, northing]
pub fn wgs84_to_utm(lon: f64, lat: f64, zone: i32, is_north: bool) -> (f64, f64) {
    let rad = core::f64::consts::PI / 180.0;
    let phi = lat * rad;
    let lambda0 = central_meridian_rad(zone);
    let dlambda = lon * rad - lambda0;

    let (sin_phi, cos_phi, tan_phi) = (phi.sin(), phi.cos(), phi.tan());
    let n = A / (1.0 - E2 * sin_phi * sin_phi).sqrt();
    let t = tan_phi * tan_phi;
    let c = ep2() * cos_phi * cos_phi;
    let aa = cos_phi * dlambda;
    let m = meridional_arc(phi);

    let (a2, a3, a4, a5, a6) = (aa * aa, aa * aa * aa, aa.powi(4), aa.powi(5), aa.powi(6));

    let easting = K0
        * n
        * (aa
            + (1.0 - t + c) * a3 / 6.0
            + (5.0 - 18.0 * t + t * t + 72.0 * c - 58.0 * ep2()) * a5 / 120.0)
        + 500000.0;

    let mut northing = K0
        * (m + n
            * tan_phi
            * (a2 / 2.0
                + (5.0 - t + 9.0 * c + 4.0 * c * c) * a4 / 24.0
                + (61.0 - 58.0 * t + t * t + 600.0 * c - 330.0 * ep2()) * a6 / 720.0));

    if !is_north {
        northing += 10000000.0;
    }
    (easting, northing)
}

/// [easting, northing] -> [lon, lat] degrees
pub fn utm_to_wgs84(easting: f64, northing: f64, zone: i32, is_north: bool) -> (f64, f64) {
    let x = easting - 500000.0;
    let y = if is_north {
        northing
    } else {
        northing - 10000000.0
    };

    let m = y / K0;
    let mu = m / (A * m1());
    let (e1, ep2) = (e1(), ep2());

    let phi1 = mu
        + (3.0 * e1 / 2.0 - 27.0 * e1.powi(3) / 32.0) * (2.0 * mu).sin()
        + (21.0 * e1 * e1 / 16.0 - 55.0 * e1.powi(4) / 32.0) * (4.0 * mu).sin()
        + (151.0 * e1.powi(3) / 96.0) * (6.0 * mu).sin()
        + (1097.0 * e1.powi(4) / 512.0) * (8.0 * mu).sin();

    let (sin1, cos1, tan1) = (phi1.sin(), phi1.cos(), phi1.tan());
    let es = E2 * sin1 * sin1;
    let n1 = A / (1.0 - es).sqrt();
    let t1 = tan1 * tan1;
    let c1 = ep2 * cos1 * cos1;
    let r1 = A * (1.0 - E2) / (1.0 - es).powf(1.5);
    let d = x / (n1 * K0);

    let (d2, d3, d4, d5, d6) = (d * d, d.powi(3), d.powi(4), d.powi(5), d.powi(6));

    let lat = phi1
        - (n1 * tan1 / r1)
            * (d2 / 2.0 - (5.0 + 3.0 * t1 + 10.0 * c1 - 4.0 * c1 * c1 - 9.0 * ep2) * d4 / 24.0
                + (61.0 + 90.0 * t1 + 298.0 * c1 + 45.0 * t1 * t1 - 252.0 * ep2 - 3.0 * c1 * c1)
                    * d6
                    / 720.0);

    let lon = (d - (1.0 + 2.0 * t1 + c1) * d3 / 6.0
        + (5.0 - 2.0 * c1 + 28.0 * t1 - 3.0 * c1 * c1 + 8.0 * ep2 + 24.0 * t1 * t1) * d5 / 120.0)
        / cos1;

    let lambda0 = central_meridian_rad(zone);
    (
        (lambda0 + lon) * 180.0 / core::f64::consts::PI,
        lat * 180.0 / core::f64::consts::PI,
    )
}

/// (zone, is_north) from a utm epsg code.
pub fn utm_params(epsg: i32) -> (i32, bool) {
    (epsg % 100, epsg < 32700)
}

/// utm epsg from an mgrs grid code ('MGRS-39RWN' or '39RWN'). cdse stac omits
/// proj:epsg; the tile encodes it (zone = leading digits, band N–X north / C–M south).
pub fn epsg_from_mgrs(grid: &str) -> i32 {
    let t = grid.strip_prefix("MGRS-").unwrap_or(grid);
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    let zone: i32 = digits.parse().unwrap_or(0);
    let band = t.as_bytes().get(digits.len()).copied().unwrap_or(b'N');
    (if band >= b'N' { 32600 } else { 32700 }) + zone
}

pub fn meters_to_degrees_lat(m: f64) -> f64 {
    m / 110540.0
}
pub fn meters_to_degrees_lon(m: f64, lat: f64) -> f64 {
    m / (111320.0 * (lat * core::f64::consts::PI / 180.0).cos())
}

/// expand a bbox [w,s,e,n] by `km` on every side (latitude-corrected).
pub fn pad_bbox(b: [f64; 4], km: f64) -> [f64; 4] {
    if km == 0.0 {
        return b;
    }
    let [w, s, e, n] = b;
    let dlat = km / 111.0;
    let dlon = km / (111.0 * ((s + n) / 2.0 * core::f64::consts::PI / 180.0).cos());
    [w - dlon, s - dlat, e + dlon, n + dlat]
}

/// approximate bbox area in km² (latitude-corrected) — the web api's abuse cap.
pub fn bbox_area_km2(b: [f64; 4]) -> f64 {
    let [w, s, e, n] = b;
    (n - s) * 111.0 * (e - w) * 111.0 * ((s + n) / 2.0 * core::f64::consts::PI / 180.0).cos()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_utm() {
        let (lon, lat) = (-102.0208, 32.0071);
        let (e, n) = wgs84_to_utm(lon, lat, 13, true);
        let (lon2, lat2) = utm_to_wgs84(e, n, 13, true);
        assert!((lon - lon2).abs() < 1e-7 && (lat - lat2).abs() < 1e-7);
    }
    #[test]
    fn mgrs_epsg() {
        assert_eq!(epsg_from_mgrs("MGRS-39RWN"), 32639);
        assert_eq!(epsg_from_mgrs("13SDV"), 32613);
        assert_eq!(epsg_from_mgrs("39CWN"), 32739); // C band → south
    }
    #[test]
    fn params() {
        assert_eq!(utm_params(32613), (13, true));
        assert_eq!(utm_params(32739), (39, false));
    }
}
