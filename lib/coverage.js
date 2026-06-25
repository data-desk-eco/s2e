// SCL clear-sky coverage pass. The symmetric twin of the detection pass: instead
// of finding hot pixels, it measures — for every catalogue site under a scene's
// footprint — whether S2 got a CLOUD-FREE look at that site on that date. This is
// the honest denominator behind persistence (n_dates / n_clear_obs); detections
// alone can't give it, because the detector skips blocks with no hot pixels and so
// leaves no record of clear-but-unlit looks.
//
// One invocation = one scene. The scene's SCL band (20 m, ~5490² uint8 ≈ 30 MB) is
// read ONCE in full and the per-site windows are sampled from memory — far cheaper
// in-region than tens of thousands of per-site byte-range reads, and bounded.
//
// Per (site, scene) we emit the full SCL class histogram (classes 0–11) over a
// small window at the site, so the cloud-free rule stays a SQL-only knob: "grab
// everything from SCL, strip down in ETL." px_valid counts the non-nodata pixels
// (the denominator); a window that is entirely nodata yields no row (S2 did not
// usably see the site there).

import { GeoTIFF } from './vendor/geotiff-esm.js';
import { wgs84ToUtm, utmParams } from './geo.js';

// Half-window in pixels: 2 => a 5×5 px (~100 m) box centred on the site. The site
// is a point; this is the localisation window, matched to the ~20–50 m hotspot
// localisation error. Tune here (the SQL is unaffected — it reads the histogram).
const HALF_WIN = 2;

// SCL has 12 classes (0–11). 3 cloud-shadow, 8 cloud med-prob, 9 cloud high-prob,
// 10 thin cirrus are the cloud classes; 0 is nodata; the cloud-free rule lives in
// SQL over the stored histogram.
const N_SCL = 12;

// Read a full single-band raster into one typed array. The fast path is a single
// byte-range readRasters() over the whole image. But geotiff's BlockedSource throws
// "Cannot read properties of undefined (reading 'offset')" on a handful of SCL COGs
// when stitching the whole image (a block index missing from the fetched set) — and
// windowed reads trip the same bug, so it is the block source, not the read size.
// The fallback downloads the entire SCL.tif (≈0.2 MB — SCL is heavily deflated) and
// reads it from an in-memory buffer, bypassing BlockedSource. Only ever runs for
// those few scenes.
async function readFullBand(image, sclUrl) {
    try {
        const r = await image.readRasters();
        return r[0];
    } catch (err) {
        if (!/reading 'offset'/.test(err.message)) throw err;
        const buf = await (await fetch(sclUrl)).arrayBuffer();
        const tiff = await GeoTIFF.fromArrayBuffer(buf);
        const img = await tiff.getImage();
        const r = await img.readRasters();
        return r[0];
    }
}

/**
 * Sample the SCL band at each in-footprint site over one scene.
 *
 * @param {object} item  normalised STAC item: { date, mgrs, id|scene, epsg,
 *   sunElevation, sunAzimuth, bands: { scl } }
 * @param {number[]} bbox  [west, south, east, north] WGS84 (unused for the read —
 *   the full tile is read — kept for signature symmetry with detectImage)
 * @param {Array<{h3, lon, lat}>} sites  catalogue sites to sample
 * @returns {Promise<Array>} one row object per (site, scene) with a `hist` array
 */
export async function coverImage(item, bbox, sites) {
    const { bands, date, epsg, mgrs } = item;
    const scene = item.id ?? item.scene ?? null;
    const sunElevation = item.sunElevation ?? item.sun_elevation ?? null;
    const sunAzimuth = item.sunAzimuth ?? item.sun_azimuth ?? null;
    const sclUrl = bands?.scl;
    if (!sclUrl || !sites?.length) return [];

    const tiff = await GeoTIFF.fromUrl(sclUrl, { allowFullFile: false });
    const image = await tiff.getImage();
    const [imgMinX, imgMinY, imgMaxX, imgMaxY] = image.getBoundingBox();
    const width = image.getWidth();
    const height = image.getHeight();
    const resX = (imgMaxX - imgMinX) / width;
    const resY = (imgMaxY - imgMinY) / height;
    const { zone, isNorth } = utmParams(epsg);

    // Read the whole SCL band once; sample every site's window from memory.
    const scl = await readFullBand(image, sclUrl);

    const rows = [];
    for (const s of sites) {
        const [easting, northing] = wgs84ToUtm(s.lon, s.lat, zone, isNorth);
        const cx = Math.floor((easting - imgMinX) / resX);
        const cy = Math.floor((imgMaxY - northing) / resY);
        if (cx < 0 || cy < 0 || cx >= width || cy >= height) continue; // outside this tile's raster

        const hist = new Array(N_SCL).fill(0);
        let valid = 0; // non-nodata pixels = the denominator
        for (let dy = -HALF_WIN; dy <= HALF_WIN; dy++) {
            const y = cy + dy;
            if (y < 0 || y >= height) continue;
            const row = y * width;
            for (let dx = -HALF_WIN; dx <= HALF_WIN; dx++) {
                const x = cx + dx;
                if (x < 0 || x >= width) continue;
                const v = scl[row + x];
                if (v >= 0 && v < N_SCL) {
                    hist[v]++;
                    if (v !== 0) valid++; // class 0 = nodata, excluded from px_valid
                }
            }
        }
        if (valid === 0) continue; // no usable SCL at the site on this date

        rows.push({
            h3: s.h3, date, mgrs, scene,
            sun_elevation: sunElevation, sun_azimuth: sunAzimuth,
            px_valid: valid, hist,
        });
    }
    return rows;
}
