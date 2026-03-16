// New detectImage implementation from optimize-s2-download branch
// Self-contained copy for A/B comparison testing

import { GeoTIFF } from '../vendor/geotiff-esm.js';
import { wgs84ToUtm, utmParams } from '../geo.js';
import { detectBlock, BLOCK_SIZE, BLOCK_OVERLAP, B12_MIN, MAX_CLOUD_LOCAL, CLOUD_FREE_THRESH } from '../detect.js';
import { readWindow, enumerateBlocks } from '../cog.js';

// B12_MIN as raw DN value (before reflectance conversion)
const B12_DN_MIN = B12_MIN * 10000 + 1000; // 4000

// How much to downsample for overview screening (8× = 160m for 20m bands)
const OVERVIEW_FACTOR = 8;

// --- Overview screening functions ---

async function screenB12Overview(b12Tiff, utmBbox, resX, resY) {
    try {
        const rasters = await b12Tiff.readRasters({
            bbox: utmBbox,
            resX: resX * OVERVIEW_FACTOR,
            resY: resY * OVERVIEW_FACTOR,
        });
        const data = rasters[0];
        for (let i = 0; i < data.length; i++) {
            if (data[i] >= B12_DN_MIN) return true;
        }
        return false;
    } catch (e) {
        return true;
    }
}

async function screenSCLOverview(sclUrl, utmBbox, resX, resY) {
    try {
        const tiff = await GeoTIFF.fromUrl(sclUrl, { allowFullFile: false });
        const rasters = await tiff.readRasters({
            bbox: utmBbox,
            resX: resX * OVERVIEW_FACTOR,
            resY: resY * OVERVIEW_FACTOR,
        });
        const data = rasters[0];
        if (!data || data.length === 0) return { skip: false, cloudFree: true };
        let cloudPixels = 0;
        for (let i = 0; i < data.length; i++) {
            const v = data[i];
            if (v === 3 || v === 8 || v === 9 || v === 10) cloudPixels++;
        }
        const cloudFrac = cloudPixels / data.length;
        if (cloudFrac > MAX_CLOUD_LOCAL) return { skip: true, cloudFree: false };
        return { skip: false, cloudFree: cloudFrac <= CLOUD_FREE_THRESH };
    } catch (e) {
        return { skip: false, cloudFree: true };
    }
}

function blockHasHotPixels(b12Raw) {
    for (let i = 0; i < b12Raw.length; i++) {
        if (b12Raw[i] >= B12_DN_MIN) return true;
    }
    return false;
}

// New detectImage with multi-layer screening
export async function detectImageNew(item, bbox, { signal } = {}) {
    const { bands, date, epsg, mgrs } = item;
    const { b12: b12Url, b11: b11Url, b8a: b8aUrl, scl: sclUrl } = bands;

    if (!b12Url || !b11Url) return { detections: [], cloudFree: true, blocksProcessed: 0, skippedOverview: false };

    const b12Tiff = await GeoTIFF.fromUrl(b12Url, { allowFullFile: false });
    const b12Image = await b12Tiff.getImage();
    const [imgMinX, imgMinY, imgMaxX, imgMaxY] = b12Image.getBoundingBox();
    const imgWidth = b12Image.getWidth();
    const imgHeight = b12Image.getHeight();
    const resX = (imgMaxX - imgMinX) / imgWidth;
    const resY = (imgMaxY - imgMinY) / imgHeight;

    const { zone, isNorth } = utmParams(epsg);
    const sw = wgs84ToUtm(bbox[0], bbox[1], zone, isNorth);
    const ne = wgs84ToUtm(bbox[2], bbox[3], zone, isNorth);
    const utmBbox = [
        Math.max(sw[0], imgMinX), Math.max(sw[1], imgMinY),
        Math.min(ne[0], imgMaxX), Math.min(ne[1], imgMaxY),
    ];

    // --- Overview screening ---

    let overviewCloudFree = true;
    if (sclUrl) {
        const cloudResult = await screenSCLOverview(sclUrl, utmBbox, resX, resY);
        if (cloudResult.skip) {
            return { detections: [], cloudFree: false, blocksProcessed: 0, skippedOverview: true };
        }
        overviewCloudFree = cloudResult.cloudFree;
    }

    const hasHot = await screenB12Overview(b12Tiff, utmBbox, resX, resY);
    if (!hasHot) {
        return { detections: [], cloudFree: overviewCloudFree, blocksProcessed: 0, skippedOverview: true };
    }

    // --- Full-res block processing ---

    const imgMeta = {
        image: b12Image, bbox: [imgMinX, imgMinY, imgMaxX, imgMaxY],
        width: imgWidth, height: imgHeight, resX, resY,
    };
    const blocks = enumerateBlocks(imgMeta, bbox, epsg);

    if (blocks.length === 0) return { detections: [], cloudFree: overviewCloudFree, blocksProcessed: 0 };

    let b11Image = null, b8aImage = null, sclImage = null;
    let bandsPromise = null;

    async function ensureBandsOpen() {
        if (!bandsPromise) {
            // Create the promise once; all concurrent callers await the same promise
            bandsPromise = (async () => {
                const promises = [];
                promises.push(
                    GeoTIFF.fromUrl(b11Url, { allowFullFile: false })
                        .then(tiff => tiff.getImage())
                        .then(img => { b11Image = img; })
                );
                if (b8aUrl) {
                    promises.push(
                        GeoTIFF.fromUrl(b8aUrl, { allowFullFile: false })
                            .then(tiff => tiff.getImage())
                            .then(img => { b8aImage = img; })
                            .catch(() => {})
                    );
                }
                if (sclUrl) {
                    promises.push(
                        GeoTIFF.fromUrl(sclUrl, { allowFullFile: false })
                            .then(tiff => tiff.getImage())
                            .then(img => { sclImage = img; })
                            .catch(() => {})
                    );
                }
                await Promise.all(promises);
            })();
        }
        await bandsPromise;
    }

    const allDetections = [];
    let allCloudFree = overviewCloudFree;
    let blocksProcessed = 0;

    const CONCURRENCY = 6;
    let idx = 0;

    async function processNext() {
        while (idx < blocks.length) {
            if (signal?.aborted) break;
            const { br, bc, window: windowArr } = blocks[idx++];
            const [x0, y0, x1, y1] = windowArr;
            const w = x1 - x0, h = y1 - y0;

            try {
                const b12Raw = await readWindow(b12Image, windowArr);
                if (!b12Raw) continue;

                if (!blockHasHotPixels(b12Raw)) {
                    blocksProcessed++;
                    continue;
                }

                await ensureBandsOpen();

                const b11Raw = await readWindow(b11Image, windowArr);
                if (!b11Raw) continue;

                let b8aRaw = null;
                if (b8aImage) {
                    try { b8aRaw = await readWindow(b8aImage, windowArr); } catch (e) { /* skip */ }
                }
                let sclRaw = null;
                if (sclImage) {
                    try { sclRaw = await readWindow(sclImage, windowArr); } catch (e) { /* skip */ }
                }

                const result = detectBlock(b12Raw, b11Raw, b8aRaw, sclRaw, {
                    date,
                    epsg,
                    imgMinX, imgMaxY, resX, resY,
                    blockOffsetX: x0,
                    blockOffsetY: y0,
                    width: w,
                    height: h,
                });

                blocksProcessed++;

                if (result.cloudFree === false) allCloudFree = false;
                for (const det of result.detections) {
                    const canonRow = Math.floor(det._peakImgRow / BLOCK_SIZE);
                    const canonCol = Math.floor(det._peakImgCol / BLOCK_SIZE);
                    if (canonRow === br && canonCol === bc) {
                        delete det._peakImgRow;
                        delete det._peakImgCol;
                        allDetections.push(det);
                    }
                }
            } catch (err) {
                console.warn(`Block error [${mgrs}_${br}_${bc}]: ${err.message}`);
            }
        }
    }

    const workers = [];
    for (let i = 0; i < Math.min(CONCURRENCY, blocks.length); i++) {
        workers.push(processNext());
    }
    await Promise.all(workers);

    return { detections: allDetections, cloudFree: allCloudFree, blocksProcessed, skippedOverview: false };
}
