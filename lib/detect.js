import { utmToWgs84, utmParams } from './geo.js';

// --- Detection thresholds (matches detect.py) ---
export const B12_MIN = 0.30;
export const B11_MIN = 0.20;
export const PEAK_B12_MIN = 0.50;
export const CONTRAST_RATIO = 3.0;
export const BACKGROUND_FLOOR = 0.15;
export const PEAKEDNESS_MIN = 1.15;
export const SATURATION = 1.0;
export const MAX_PIXELS = 80;
export const LARGE_PIXELS = 30;
export const LARGE_B12_MIN = 0.70;
export const WARM_FRACTION = 0.5;
export const WARM_MAX_PIXELS = 100;
export const MAX_CLOUD_LOCAL = 0.75;
export const CLOUD_FREE_THRESH = 0.3;

export const BLOCK_SIZE = 256;
export const BLOCK_OVERLAP = 10;

// --- Pure functions ---

export function dnToReflectance(dn) {
    return (dn - 1000) / 10000;
}

/**
 * Check cloud fraction from SCL raw data.
 * @param {Uint16Array} sclRaw - raw SCL pixel values
 * @param {number} width
 * @param {number} height
 * @returns {{ skip: boolean, cloudFree: boolean }}
 */
export function screenClouds(sclRaw, width, height) {
    const total = sclRaw.length;
    if (total === 0) return { skip: false, cloudFree: true };

    let cloudPixels = 0;
    for (let i = 0; i < total; i++) {
        const v = sclRaw[i];
        if (v === 3 || v === 8 || v === 9 || v === 10) cloudPixels++;
    }

    const cloudFrac = cloudPixels / total;
    if (cloudFrac > MAX_CLOUD_LOCAL) return { skip: true, cloudFree: false };
    return { skip: false, cloudFree: cloudFrac <= CLOUD_FREE_THRESH };
}

export function labelConnectedComponents(mask, width, height) {
    const labels = new Int32Array(width * height);
    let nextLabel = 1;
    for (let i = 0; i < mask.length; i++) {
        if (!mask[i] || labels[i]) continue;
        const queue = [i];
        labels[i] = nextLabel;
        let head = 0;
        while (head < queue.length) {
            const idx = queue[head++];
            const r = Math.floor(idx / width);
            const c = idx % width;
            const neighbors = [];
            if (r > 0) neighbors.push(idx - width);
            if (r < height - 1) neighbors.push(idx + width);
            if (c > 0) neighbors.push(idx - 1);
            if (c < width - 1) neighbors.push(idx + 1);
            for (const n of neighbors) {
                if (mask[n] && !labels[n]) {
                    labels[n] = nextLabel;
                    queue.push(n);
                }
            }
        }
        nextLabel++;
    }
    return { labels, count: nextLabel - 1 };
}

/**
 * Run the full Sentinel-2 flare detection pipeline on a single block.
 *
 * @param {Uint16Array} b12Raw - raw B12 DN values (w*h)
 * @param {Uint16Array} b11Raw - raw B11 DN values (w*h)
 * @param {Uint16Array|null} b8aRaw - raw B8A DN values (w*h), or null
 * @param {Uint16Array|null} sclRaw - raw SCL values (w*h), or null
 * @param {object} meta
 * @param {string} meta.date          - image date string
 * @param {number} meta.epsg          - UTM EPSG code
 * @param {number} meta.imgMinX       - image left edge in UTM (full-image origin)
 * @param {number} meta.imgMaxY       - image top edge in UTM (full-image origin)
 * @param {number} meta.resX          - pixel width in meters
 * @param {number} meta.resY          - pixel height in meters (positive)
 * @param {number} meta.blockOffsetX  - x0: column offset of block within full image
 * @param {number} meta.blockOffsetY  - y0: row offset of block within full image
 * @param {number} meta.width         - block width in pixels
 * @param {number} meta.height        - block height in pixels
 * @returns {{ detections: object[], cloudFree: boolean }}
 */
export function detectBlock(b12Raw, b11Raw, b8aRaw, sclRaw, meta) {
    const { date, epsg, imgMinX, imgMaxY, resX, resY,
            blockOffsetX, blockOffsetY, width: w, height: h } = meta;

    if (w <= 0 || h <= 0) return { detections: [], cloudFree: false };

    // 1. Cloud check via SCL
    let blockCloudFree = true;
    if (sclRaw) {
        const { skip, cloudFree } = screenClouds(sclRaw, w, h);
        if (skip) return { detections: [], cloudFree: false };
        blockCloudFree = cloudFree;
    }

    // 2. Convert B12 DN to reflectance, compute background
    const n = w * h;
    const b12 = new Float32Array(n);
    const bgPixels = [];
    for (let i = 0; i < n; i++) {
        const v = (b12Raw[i] - 1000) / 10000;
        b12[i] = v;
        if (v < B12_MIN) bgPixels.push(v);
    }
    if (bgPixels.length < 10) return { detections: [], cloudFree: blockCloudFree };
    bgPixels.sort((a, b) => a - b);
    const medianBg = bgPixels[Math.floor(bgPixels.length / 2)];
    const contrastThresh = Math.max(medianBg, BACKGROUND_FLOOR) * CONTRAST_RATIO;

    // 3. Convert B11, build mask
    const b11 = new Float32Array(n);
    const mask = new Uint8Array(n);
    let anyMask = false;
    const hasB8a = !!b8aRaw;
    for (let i = 0; i < n; i++) {
        const b11v = (b11Raw[i] - 1000) / 10000;
        b11[i] = b11v;
        const b12v = b12[i];
        if (b12v <= B12_MIN || b11v <= B11_MIN) continue;
        if (b12v <= contrastThresh) continue;
        if (hasB8a) {
            const b8av = (b8aRaw[i] - 1000) / 10000;
            const denom = b11v + b8av;
            const nhiswnir = denom > 0.01 ? (b11v - b8av) / denom : 0;
            if (!(nhiswnir > 0 || b11v > SATURATION || b12v > SATURATION)) continue;
        } else {
            if (b11v <= SATURATION) continue;
        }
        mask[i] = 1;
        anyMask = true;
    }
    if (!anyMask) return { detections: [], cloudFree: blockCloudFree };

    // 4. Label connected components
    const { labels, count } = labelConnectedComponents(mask, w, h);
    if (count === 0) return { detections: [], cloudFree: blockCloudFree };

    // 5. Compute UTM bounds for this block
    const x0 = blockOffsetX, y0 = blockOffsetY;
    const x1 = x0 + w, y1 = y0 + h;
    const utmMinX = imgMinX + x0 * resX;
    const utmMinY = imgMaxY - y1 * resY;
    const utmMaxX = imgMinX + x1 * resX;
    const utmMaxY_w = imgMaxY - y0 * resY;

    const { zone, isNorth } = utmParams(epsg);

    // 6. Per-component filtering and detection output
    const detections = [];
    for (let labelId = 1; labelId <= count; labelId++) {
        let nPixels = 0;
        let peakB12 = -Infinity;
        let peakIdx = -1;
        let sumB12 = 0;
        for (let i = 0; i < n; i++) {
            if (labels[i] !== labelId) continue;
            nPixels++;
            sumB12 += b12[i];
            if (b12[i] > peakB12) { peakB12 = b12[i]; peakIdx = i; }
        }
        if (nPixels > MAX_PIXELS) continue;
        if (peakB12 < PEAK_B12_MIN) continue;
        if (nPixels > LARGE_PIXELS && peakB12 < LARGE_B12_MIN) continue;
        const avgB12 = sumB12 / nPixels;
        if (nPixels > 1 && peakB12 < PEAKEDNESS_MIN * avgB12 && avgB12 < SATURATION) continue;
        if (nPixels === 1 && peakB12 < 0.65) continue;

        // Warm region filter
        const peakRow = Math.floor(peakIdx / w);
        const peakCol = peakIdx % w;
        const warmThresh = peakB12 * WARM_FRACTION;
        let warmSize = 0;
        if (b12[peakIdx] > warmThresh) {
            const visited = new Uint8Array(n);
            const q = [peakIdx];
            visited[peakIdx] = 1;
            let head = 0;
            while (head < q.length) {
                warmSize++;
                if (warmSize > WARM_MAX_PIXELS) break;
                const idx = q[head++];
                const r = Math.floor(idx / w), c = idx % w;
                if (r > 0 && !visited[idx - w] && b12[idx - w] > warmThresh) { visited[idx - w] = 1; q.push(idx - w); }
                if (r < h - 1 && !visited[idx + w] && b12[idx + w] > warmThresh) { visited[idx + w] = 1; q.push(idx + w); }
                if (c > 0 && !visited[idx - 1] && b12[idx - 1] > warmThresh) { visited[idx - 1] = 1; q.push(idx - 1); }
                if (c < w - 1 && !visited[idx + 1] && b12[idx + 1] > warmThresh) { visited[idx + 1] = 1; q.push(idx + 1); }
            }
        }
        if (warmSize > WARM_MAX_PIXELS) continue;

        // Convert peak pixel to WGS84
        const colFrac = (peakCol + 0.5) / w;
        const rowFrac = (peakRow + 0.5) / h;
        const utmX = utmMinX + colFrac * (utmMaxX - utmMinX);
        const utmY = utmMaxY_w - rowFrac * (utmMaxY_w - utmMinY);
        const [lon, lat] = utmToWgs84(utmX, utmY, zone, isNorth);

        detections.push({
            lon,
            lat,
            max_b12: peakB12,
            avg_b12: avgB12,
            pixels: nPixels,
            date,
            _peakImgRow: y0 + peakRow,
            _peakImgCol: x0 + peakCol,
        });
    }
    return { detections, cloudFree: blockCloudFree };
}
