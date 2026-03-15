#!/usr/bin/env node

// Benchmark: old vs new detectImage on Ras Laffan (Qatar gas flare complex)
// Run locally in a browser-like Node env or adapt for test/manual.html
//
// Ras Laffan Industrial City: ~25.9°N, 51.5°E
// This is an excellent test case: dense persistent gas flares surrounded by
// mostly cold ocean/desert — should see big wins from overview screening.

globalThis.self = globalThis;
globalThis.Worker = class {};

const geotiffMod = await import('../vendor/geotiff.js');
globalThis.GeoTIFF = geotiffMod.default;
self.GeoTIFF = geotiffMod.default;

const { searchSTAC } = await import('../stac.js');
const { detectImage } = await import('../cog.js');
const { detectBlock, BLOCK_SIZE, BLOCK_OVERLAP, B12_MIN } = await import('../detect.js');
const { wgs84ToUtm, utmParams } = await import('../geo.js');

const { GeoTIFF: GeoTIFFLib } = geotiffMod.default;

// --- Old detectImage (pre-optimization baseline) ---

async function detectImageOld(item, bbox, { signal } = {}) {
    const { bands, date, epsg, mgrs } = item;
    const { b12: b12Url, b11: b11Url, b8a: b8aUrl, scl: sclUrl } = bands;
    if (!b12Url || !b11Url) return { detections: [], cloudFree: true, blocksProcessed: 0, bandReads: 0 };

    const tiff = await GeoTIFFLib.fromUrl(b12Url, { allowFullFile: false });
    const image = await tiff.getImage();
    const [minX, minY, maxX, maxY] = image.getBoundingBox();
    const b12Meta = {
        image, bbox: [minX, minY, maxX, maxY],
        width: image.getWidth(), height: image.getHeight(),
        resX: (maxX - minX) / image.getWidth(),
        resY: (maxY - minY) / image.getHeight(),
    };

    const { zone, isNorth } = utmParams(epsg);
    const sw = wgs84ToUtm(bbox[0], bbox[1], zone, isNorth);
    const ne = wgs84ToUtm(bbox[2], bbox[3], zone, isNorth);
    const imgWidth = b12Meta.width, imgHeight = b12Meta.height;
    const [imgMinX, imgMinY2, imgMaxX, imgMaxY] = b12Meta.bbox;
    const resX = b12Meta.resX, resY = b12Meta.resY;

    const px0 = Math.max(0, Math.floor((Math.max(sw[0], imgMinX) - imgMinX) / resX));
    const py0 = Math.max(0, Math.floor((imgMaxY - Math.min(ne[1], imgMaxY)) / resY));
    const px1 = Math.min(imgWidth, Math.ceil((Math.min(ne[0], imgMaxX) - imgMinX) / resX));
    const py1 = Math.min(imgHeight, Math.ceil((imgMaxY - Math.max(sw[1], imgMinY2)) / resY));
    if (px1 <= px0 || py1 <= py0) return { detections: [], cloudFree: true, blocksProcessed: 0, bandReads: 0 };

    const blockRow0 = Math.floor(py0 / BLOCK_SIZE);
    const blockRow1 = Math.ceil(py1 / BLOCK_SIZE);
    const blockCol0 = Math.floor(px0 / BLOCK_SIZE);
    const blockCol1 = Math.ceil(px1 / BLOCK_SIZE);
    const blocks = [];
    for (let br = blockRow0; br < blockRow1; br++) {
        for (let bc = blockCol0; bc < blockCol1; bc++) {
            const x0 = Math.max(0, bc * BLOCK_SIZE - BLOCK_OVERLAP);
            const y0 = Math.max(0, br * BLOCK_SIZE - BLOCK_OVERLAP);
            const x1 = Math.min(imgWidth, (bc + 1) * BLOCK_SIZE + BLOCK_OVERLAP);
            const y1 = Math.min(imgHeight, (br + 1) * BLOCK_SIZE + BLOCK_OVERLAP);
            blocks.push({ br, bc, window: [x0, y0, x1, y1] });
        }
    }
    if (blocks.length === 0) return { detections: [], cloudFree: true, blocksProcessed: 0, bandReads: 0 };

    // Open all bands upfront (old behavior)
    let b11Image = null, b8aImage = null, sclImage = null;
    const promises = [];
    promises.push(GeoTIFFLib.fromUrl(b11Url, { allowFullFile: false }).then(t => t.getImage()).then(i => { b11Image = i; }));
    if (b8aUrl) promises.push(GeoTIFFLib.fromUrl(b8aUrl, { allowFullFile: false }).then(t => t.getImage()).then(i => { b8aImage = i; }).catch(() => {}));
    if (sclUrl) promises.push(GeoTIFFLib.fromUrl(sclUrl, { allowFullFile: false }).then(t => t.getImage()).then(i => { sclImage = i; }).catch(() => {}));
    await Promise.all(promises);

    const allDetections = [];
    let allCloudFree = true;
    let blocksProcessed = 0;
    let bandReads = 0;
    const CONCURRENCY = 6;
    let idx = 0;

    async function processNext() {
        while (idx < blocks.length) {
            if (signal?.aborted) break;
            const { br, bc, window: windowArr } = blocks[idx++];
            const [x0, y0, x1, y1] = windowArr;
            const w = x1 - x0, h = y1 - y0;
            try {
                const rB12 = await image.readRasters({ window: windowArr }); bandReads++;
                const b12Raw = rB12[0]; if (!b12Raw) continue;
                const rB11 = await b11Image.readRasters({ window: windowArr }); bandReads++;
                const b11Raw = rB11[0]; if (!b11Raw) continue;
                let b8aRaw = null;
                if (b8aImage) { try { const r = await b8aImage.readRasters({ window: windowArr }); b8aRaw = r[0]; bandReads++; } catch {} }
                let sclRaw = null;
                if (sclImage) { try { const r = await sclImage.readRasters({ window: windowArr }); sclRaw = r[0]; bandReads++; } catch {} }

                const result = detectBlock(b12Raw, b11Raw, b8aRaw, sclRaw, {
                    date, epsg, imgMinX, imgMaxY, resX, resY,
                    blockOffsetX: x0, blockOffsetY: y0, width: w, height: h,
                });
                blocksProcessed++;
                if (result.cloudFree === false) allCloudFree = false;
                for (const det of result.detections) {
                    const canonRow = Math.floor(det._peakImgRow / BLOCK_SIZE);
                    const canonCol = Math.floor(det._peakImgCol / BLOCK_SIZE);
                    if (canonRow === br && canonCol === bc) {
                        delete det._peakImgRow; delete det._peakImgCol;
                        allDetections.push(det);
                    }
                }
            } catch (err) {
                console.warn(`  Old block error [${mgrs}_${br}_${bc}]: ${err.message}`);
            }
        }
    }
    const workers = [];
    for (let i = 0; i < Math.min(CONCURRENCY, blocks.length); i++) workers.push(processNext());
    await Promise.all(workers);
    return { detections: allDetections, cloudFree: allCloudFree, blocksProcessed, bandReads };
}

// --- Main benchmark ---

const RAS_LAFFAN_BBOX = [51.44, 25.84, 51.62, 25.98];
const end = new Date().toISOString().slice(0, 10);
const start = new Date(Date.now() - 90 * 86400000).toISOString().slice(0, 10);

console.log(`\n${'='.repeat(70)}`);
console.log(`  Ras Laffan benchmark: ${start} to ${end}`);
console.log(`  bbox: ${JSON.stringify(RAS_LAFFAN_BBOX)}`);
console.log(`${'='.repeat(70)}\n`);

console.log('Searching STAC...');
const items = [];
for await (const item of searchSTAC(RAS_LAFFAN_BBOX, start, end)) {
    items.push(item);
}
console.log(`Found ${items.length} scenes\n`);

const MAX_SCENES = parseInt(process.argv[2]) || 8;
const testItems = items.slice(0, MAX_SCENES);
console.log(`Testing ${testItems.length} scenes (pass count as argv[1] to change):\n`);

let oldTotalMs = 0, newTotalMs = 0;
let oldTotalDet = 0, newTotalDet = 0;
let oldTotalBandReads = 0;
let newScenesSkipped = 0;
let allMatch = true;

for (const item of testItems) {
    const label = `${item.date} (${item.mgrs}, cloud=${item.cloudCover?.toFixed(0)}%)`;

    const t0old = performance.now();
    const oldRes = await detectImageOld(item, RAS_LAFFAN_BBOX);
    const oldMs = performance.now() - t0old;

    const t0new = performance.now();
    const newRes = await detectImage(item, RAS_LAFFAN_BBOX);
    const newMs = performance.now() - t0new;

    oldTotalMs += oldMs;
    newTotalMs += newMs;
    oldTotalDet += oldRes.detections.length;
    newTotalDet += newRes.detections.length;
    oldTotalBandReads += oldRes.bandReads;
    if (newRes.skippedOverview) newScenesSkipped++;

    const match = oldRes.detections.length === newRes.detections.length;
    if (!match) allMatch = false;
    const skip = newRes.skippedOverview ? ' [OVERVIEW SKIP]' : '';
    const parity = match ? '' : ` *** MISMATCH old=${oldRes.detections.length} new=${newRes.detections.length}`;

    console.log(`${label}`);
    console.log(`  OLD: ${oldMs.toFixed(0)}ms  ${oldRes.detections.length} det  ${oldRes.bandReads} band reads`);
    console.log(`  NEW: ${newMs.toFixed(0)}ms  ${newRes.detections.length} det  blocks=${newRes.blocksProcessed}${skip}${parity}`);
    console.log(`  ${(oldMs / Math.max(newMs, 1)).toFixed(1)}× faster\n`);
}

console.log(`${'='.repeat(70)}`);
console.log(`  OLD total: ${(oldTotalMs / 1000).toFixed(1)}s, ${oldTotalDet} detections, ${oldTotalBandReads} band reads`);
console.log(`  NEW total: ${(newTotalMs / 1000).toFixed(1)}s, ${newTotalDet} detections, ${newScenesSkipped}/${testItems.length} scenes skipped`);
console.log(`  Speedup: ${(oldTotalMs / Math.max(newTotalMs, 1)).toFixed(1)}×`);
console.log(`  Detection parity: ${allMatch ? 'PASS' : 'MISMATCH'}`);
console.log(`${'='.repeat(70)}\n`);
