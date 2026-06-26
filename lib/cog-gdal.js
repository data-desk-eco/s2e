// node-only sibling of cog.js: reads Sentinel-2 .jp2 bands from the Copernicus
// `eodata` archive via gdal-async — windowed /vsis3/eodata reads, JP2OpenJPEG
// decode — and returns the exact typed arrays + result shape detect.js / runAOI
// expect. geotiff.js can't read JP2, so on the EU-sovereign CloudFerro box this
// replaces cog.js as the I/O shell. detect.js and the browser/AWS COG path are
// untouched; select this reader by passing { detect: detectImage } to runAOI.
import gdalPkg from 'gdal-async';
import { wgs84ToUtm, utmParams } from './geo.js';
import { enumerateBlocks } from './cog.js';
import { detectBlock, resolveThresholds, BLOCK_SIZE } from './detect.js';

const gdal = gdalPkg.default ?? gdalPkg;

// /vsis3 against eodata: in-region, keyed (S3 creds in env). Set GDAL's S3
// endpoint + path-style addressing once; tune the curl cache for windowed reads.
for (const [k, v] of Object.entries({
    AWS_S3_ENDPOINT: process.env.AWS_S3_ENDPOINT || 'eodata.dataspace.copernicus.eu',
    AWS_VIRTUAL_HOSTING: 'FALSE', AWS_HTTPS: 'YES',
    GDAL_DISABLE_READDIR_ON_OPEN: 'EMPTY_DIR',
    GDAL_HTTP_MULTIPLEX: 'YES', VSI_CACHE: 'TRUE',
})) gdal.config.set(k, v);

const OVERVIEW_FACTOR = 8;                       // 8× = 160 m screen for 20 m bands
const b12DnMin = (T) => T.b12Min * 10000 + 1000; // mask floor as a raw DN value

// Sentinel-2 baseline N0400 (acquisitions ≥ 2022-01-25) bakes a +1000
// BOA_ADD_OFFSET into the raw .jp2 DN. The Element84 COGs the methodology was
// tuned on have it harmonised out, so eodata JP2 spectral DN must be shifted to
// match — else the same scene over-detects (verified: every pixel off by exactly
// 1000, 3228→2339 dets, blobs 18→34841 px). SCL (a class map) is left untouched.
const harmonizeOffset = (date) => (date >= '2022-01-25' ? 1000 : 0);
function harmonize(a, off) {
    if (off && a) for (let i = 0; i < a.length; i++) a[i] = a[i] > off ? a[i] - off : 0;
    return a;
}

// s3://eodata/… → /vsis3/eodata/… ; https://… → /vsicurl/… ; /vsi* and local
// paths pass through. One reader serves eodata on the box and any gdal-openable
// url/file under test.
export function toVsiPath(href) {
    if (href.startsWith('s3://')) return '/vsis3/' + href.slice(5);
    if (/^https?:\/\//.test(href)) return '/vsicurl/' + href;
    return href;
}

// Open a band → handle + the same metadata cog.js's openCOG returns.
export async function openCOG(href) {
    const ds = await gdal.openAsync(toVsiPath(href));
    const gt = await ds.geoTransformAsync;
    const { x: width, y: height } = await ds.rasterSizeAsync;
    const band = await ds.bands.getAsync(1);
    const resX = gt[1], resY = -gt[5], minX = gt[0], maxY = gt[3];
    return { ds, band, width, height, resX, resY,
        bbox: [minX, maxY - height * resY, minX + width * resX, maxY] };
}

// Read pixel window [x0,y0,x1,y1] at native res → Uint16Array (null if empty).
export async function readWindow(img, [x0, y0, x1, y1]) {
    const w = x1 - x0, h = y1 - y0;
    if (w <= 0 || h <= 0) return null;
    return img.band.pixels.readAsync(x0, y0, w, h);
}

// Read the clipped UTM query bbox decimated by `factor` → Uint16Array, for cheap
// overview screening before any full-res JP2 decode (JP2 resolution levels make
// this near-free). Returns null if the bbox falls outside the tile.
async function readOverview(img, utmBbox, factor) {
    const [minX, , , maxY] = img.bbox;
    const x0 = Math.max(0, Math.floor((utmBbox[0] - minX) / img.resX));
    const y0 = Math.max(0, Math.floor((maxY - utmBbox[3]) / img.resY));
    const x1 = Math.min(img.width, Math.ceil((utmBbox[2] - minX) / img.resX));
    const y1 = Math.min(img.height, Math.ceil((maxY - utmBbox[1]) / img.resY));
    const w = x1 - x0, h = y1 - y0;
    if (w <= 0 || h <= 0) return null;
    const bw = Math.max(1, Math.round(w / factor)), bh = Math.max(1, Math.round(h / factor));
    return img.band.pixels.readAsync(x0, y0, w, h, null, { buffer_width: bw, buffer_height: bh });
}

const anyHot = (a, floor) => { for (let i = 0; i < a.length; i++) if (a[i] >= floor) return true; return false; };
const cloudFrac = (a) => { let c = 0; for (let i = 0; i < a.length; i++) { const v = a[i]; if (v === 3 || v === 8 || v === 9 || v === 10) c++; } return a.length ? c / a.length : 0; };

// Process one S2 scene from CDSE: same contract as cog.js detectImage.
// item: normalized STAC item (stac.js, source:'cdse') with .bands.{b12,b11,b8a,scl}
// s3://eodata hrefs, .date, .epsg, .mgrs, .id, sun angles. bbox: [W,S,E,N] WGS84.
// Returns { detections, cloudFree, blocksProcessed, skippedOverview }.
export async function detectImage(item, bbox, { signal, thresholds, screenOverview = true, harmonize: doHarmonize = true } = {}) {
    const T = thresholds ?? resolveThresholds();
    const { bands, date, epsg, mgrs, id: scene } = item;
    // raw eodata JP2 needs the offset removed; an already-harmonised Element84 COG
    // (the aws path, used in tests) must not be shifted again.
    const off = doHarmonize ? harmonizeOffset(date) : 0;
    const sunElevation = item.sunElevation ?? item.sun_elevation ?? null;
    const sunAzimuth = item.sunAzimuth ?? item.sun_azimuth ?? null;
    const { b12: b12Url, b11: b11Url, b8a: b8aUrl, scl: sclUrl } = bands;
    if (!b12Url || !b11Url) return { detections: [], cloudFree: true, blocksProcessed: 0 };

    const b12 = await openCOG(b12Url);
    const [imgMinX, imgMinY, imgMaxX, imgMaxY] = b12.bbox;
    const { resX, resY, width: imgWidth, height: imgHeight } = b12;

    const { zone, isNorth } = utmParams(epsg);
    const sw = wgs84ToUtm(bbox[0], bbox[1], zone, isNorth);
    const ne = wgs84ToUtm(bbox[2], bbox[3], zone, isNorth);
    const utmBbox = [Math.max(sw[0], imgMinX), Math.max(sw[1], imgMinY),
        Math.min(ne[0], imgMaxX), Math.min(ne[1], imgMaxY)];

    // Overview screen: skip fully-cloudy or hot-pixel-free scenes before full-res.
    let overviewCloudFree = true;
    if (screenOverview) {
        if (sclUrl) try {
            const frac = cloudFrac(await readOverview(await openCOG(sclUrl), utmBbox, OVERVIEW_FACTOR) ?? []);
            if (frac > T.maxCloudLocal) return { detections: [], cloudFree: false, blocksProcessed: 0, skippedOverview: true };
            overviewCloudFree = frac <= T.cloudFreeThresh;
        } catch { /* screen best-effort */ }
        const ov = harmonize(await readOverview(b12, utmBbox, OVERVIEW_FACTOR), off);
        if (ov && !anyHot(ov, b12DnMin(T)))
            return { detections: [], cloudFree: overviewCloudFree, blocksProcessed: 0, skippedOverview: true };
    }

    const blocks = enumerateBlocks({ width: imgWidth, height: imgHeight, bbox: b12.bbox, resX, resY }, bbox, epsg);
    if (!blocks.length) return { detections: [], cloudFree: overviewCloudFree, blocksProcessed: 0 };

    // Auxiliary bands opened once, lazily — only if some block has hot B12.
    let aux = null;
    const ensureAux = () => (aux ??= (async () => ({
        b11: await openCOG(b11Url),
        b8a: b8aUrl ? await openCOG(b8aUrl).catch(() => null) : null,
        scl: sclUrl ? await openCOG(sclUrl).catch(() => null) : null,
    }))());

    const allDetections = [];
    let allCloudFree = overviewCloudFree, blocksProcessed = 0, idx = 0;
    const floor = b12DnMin(T);

    async function worker() {
        while (idx < blocks.length) {
            if (signal?.aborted) break;
            const { br, bc, window: win } = blocks[idx++];
            const [x0, y0, x1, y1] = win, w = x1 - x0, h = y1 - y0;
            try {
                const b12Raw = harmonize(await readWindow(b12, win), off);
                if (!b12Raw) continue;
                if (!anyHot(b12Raw, floor)) { blocksProcessed++; continue; }
                const a = await ensureAux();
                const b11Raw = harmonize(await readWindow(a.b11, win), off);
                if (!b11Raw) continue;
                const b8aRaw = a.b8a ? harmonize(await readWindow(a.b8a, win).catch(() => null), off) : null;
                const sclRaw = a.scl ? await readWindow(a.scl, win).catch(() => null) : null;
                const result = detectBlock(b12Raw, b11Raw, b8aRaw, sclRaw, {
                    date, epsg, mgrs, scene, imgMinX, imgMaxY, resX, resY,
                    blockOffsetX: x0, blockOffsetY: y0, width: w, height: h,
                    sunElevation, sunAzimuth }, T);
                blocksProcessed++;
                if (result.cloudFree === false) allCloudFree = false;
                for (const det of result.detections) {
                    if (Math.floor(det._peakImgRow / BLOCK_SIZE) === br &&
                        Math.floor(det._peakImgCol / BLOCK_SIZE) === bc) {
                        delete det._peakImgRow; delete det._peakImgCol;
                        allDetections.push(det);
                    }
                }
            } catch (err) {
                console.warn(`Block error [${mgrs}_${br}_${bc}]: ${err.message}`);
            }
        }
    }
    await Promise.all(Array.from({ length: Math.min(6, blocks.length) }, worker));
    return { detections: allDetections, cloudFree: allCloudFree, blocksProcessed };
}
