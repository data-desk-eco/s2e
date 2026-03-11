import { GeoTIFF } from './vendor/geotiff-esm.js';
import { wgs84ToUtm, utmParams } from './geo.js';
import { detectBlock, BLOCK_SIZE, BLOCK_OVERLAP } from './detect.js';

// Open a COG and return image handle + metadata
export async function openCOG(url) {
    const tiff = await GeoTIFF.fromUrl(url, { allowFullFile: false });
    const image = await tiff.getImage();
    const [minX, minY, maxX, maxY] = image.getBoundingBox();
    return {
        image,
        bbox: [minX, minY, maxX, maxY],
        width: image.getWidth(),
        height: image.getHeight(),
        resX: (maxX - minX) / image.getWidth(),
        resY: (maxY - minY) / image.getHeight(),
    };
}

// Read a window from a COG image, returns typed array
export async function readWindow(image, windowArr) {
    const [x0, y0, x1, y1] = windowArr;
    if (x1 - x0 <= 0 || y1 - y0 <= 0) return null;
    const rasters = await image.readRasters({ window: windowArr });
    return rasters[0]; // Return just the typed array, not {data, width, height}
}

// Enumerate blocks overlapping a bbox (in image pixel coordinates)
// imgMeta: result of openCOG(); bbox: [west, south, east, north] WGS84; epsg: image EPSG
// Returns array of { br, bc, window: [x0, y0, x1, y1] }
export function enumerateBlocks(imgMeta, bbox, epsg) {
    const { width: imgWidth, height: imgHeight, bbox: imgBbox, resX, resY } = imgMeta;
    const [imgMinX, imgMinY, imgMaxX, imgMaxY] = imgBbox;

    const { zone, isNorth } = utmParams(epsg);
    const sw = wgs84ToUtm(bbox[0], bbox[1], zone, isNorth);
    const ne = wgs84ToUtm(bbox[2], bbox[3], zone, isNorth);

    const px0 = Math.max(0, Math.floor((Math.max(sw[0], imgMinX) - imgMinX) / resX));
    const py0 = Math.max(0, Math.floor((imgMaxY - Math.min(ne[1], imgMaxY)) / resY));
    const px1 = Math.min(imgWidth, Math.ceil((Math.min(ne[0], imgMaxX) - imgMinX) / resX));
    const py1 = Math.min(imgHeight, Math.ceil((imgMaxY - Math.max(sw[1], imgMinY)) / resY));

    if (px1 <= px0 || py1 <= py0) return [];

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
    return blocks;
}

// Process a full S2 image: open bands, enumerate blocks, run detection, dedup
// item: normalized STAC item (from stac.js) with .bands.b12/.b11/.b8a/.scl, .date, .epsg, .mgrs
// bbox: [west, south, east, north] WGS84
// Returns { detections: [...], cloudFree: boolean, blocksProcessed: number }
export async function detectImage(item, bbox, { signal } = {}) {
    const { bands, date, epsg, mgrs } = item;
    const { b12: b12Url, b11: b11Url, b8a: b8aUrl, scl: sclUrl } = bands;

    if (!b12Url || !b11Url) return { detections: [], cloudFree: true, blocksProcessed: 0 };

    const b12Meta = await openCOG(b12Url);
    const blocks = enumerateBlocks(b12Meta, bbox, epsg);

    if (blocks.length === 0) return { detections: [], cloudFree: true, blocksProcessed: 0 };

    // Open auxiliary bands lazily
    let b11Image = null, b8aImage = null, sclImage = null;
    let bandsOpened = false;

    async function ensureBandsOpen() {
        if (bandsOpened) return;
        bandsOpened = true;
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
    }

    await ensureBandsOpen();

    const { image: b12Image, width: imgWidth, height: imgHeight,
            bbox: imgBbox, resX, resY } = b12Meta;
    const [imgMinX, imgMinY, imgMaxX, imgMaxY] = imgBbox;

    const allDetections = [];
    let allCloudFree = true;
    let blocksProcessed = 0;

    const CONCURRENCY = 6;
    let idx = 0;

    async function processNext() {
        while (idx < blocks.length) {
            if (signal?.aborted) break;
            const { br, bc, window: windowArr } = blocks[idx++];

            try {
                const result = await detectBlock({
                    b12Image, b11Image, b8aImage, sclImage,
                    windowArr,
                    imgDate: date,
                    sunElevation: item.sunElevation ?? null,
                    itemEpsg: epsg,
                    imgMinX, imgMaxY, resX, resY,
                    blockId: `${mgrs}_${br}_${bc}`,
                    b12Url,
                });

                blocksProcessed++;

                if (!result.skipped) {
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

    return { detections: allDetections, cloudFree: allCloudFree, blocksProcessed };
}
