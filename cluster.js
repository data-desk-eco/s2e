const DEG_TO_RAD = Math.PI / 180;
const R_EARTH = 6371000;

function fastDistM(lat1, lon1, lat2, lon2) {
    const dLat = (lat2 - lat1) * DEG_TO_RAD;
    const dLon = (lon2 - lon1) * DEG_TO_RAD * Math.cos(((lat1 + lat2) * 0.5) * DEG_TO_RAD);
    return R_EARTH * Math.sqrt(dLat * dLat + dLon * dLon);
}

/**
 * Check whether all detections fall within April–August (months 3–7, 0-indexed).
 * @param {Array<{date: string}>} detections
 * @returns {boolean}
 */
export function isSeasonal(detections) {
    const sunny = new Set([3, 4, 5, 6, 7]); // Apr(3)–Aug(7) (JS months 0-indexed)
    const months = new Set(detections.map(d => new Date(d.date + 'T00:00').getUTCMonth()));
    for (const m of months) { if (!sunny.has(m)) return false; }
    return true;
}

/**
 * Spatially cluster cross-date detections into persistent flare sites.
 *
 * @param {Array<object>} detections - Per-date detection records. Coordinate fields
 *   may be `lon`/`lat` (s2-flares) or `flare_lon`/`flare_lat` (burnoff); both are
 *   handled. Required fields: lon/lat (or flare_lon/flare_lat), date, max_b12, pixels.
 * @param {object} [options]
 * @param {number} [options.mergeDistance=135] - Merge radius in metres.
 * @param {number} [options.minDates=4] - Minimum unique detection dates to keep a cluster.
 * @param {number} [options.minAvgB12=0.85] - Minimum mean B12 (per-date max) to keep a cluster.
 * @param {Map<string, {cloudFree: boolean}>|null} [options.observations=null] - Optional
 *   map from date strings to observation metadata. When provided, persistence is computed
 *   as detection_count / cloud_free_observations. When null, persistence is null.
 * @returns {Array<object>} Array of cluster objects.
 */
export function clusterDetections(detections, options = {}) {
    const {
        mergeDistance = 135,
        minDates = 4,
        minAvgB12 = 0.85,
        observations = null,
    } = options;

    if (detections.length === 0) return [];

    if (mergeDistance === 0) {
        const clusters = [];
        for (const det of detections) {
            if (det.max_b12 < minAvgB12) continue;
            const lon = det.lon ?? det.flare_lon;
            const lat = det.lat ?? det.flare_lat;
            clusters.push({
                lon,
                lat,
                max_b12: det.max_b12,
                avg_b12: det.max_b12,
                detection_count: 1,
                date_count: 1,
                first_date: det.date,
                last_date: det.date,
                persistence: null,
                seasonal: isSeasonal([det]),
                detections: [{ date: det.date, max_b12: det.max_b12, pixels: det.pixels, lon, lat }],
            });
        }
        return clusters;
    }

    // Sort by max_b12 descending so highest-intensity detections become anchors.
    const sorted = detections.slice().sort((a, b) => b.max_b12 - a.max_b12);

    const CELL_DEG = mergeDistance / 111320;
    const grid = new Map();
    const clusterList = [];
    const KEY_SHIFT = 0x100000;

    for (const det of sorted) {
        const lon = det.lon ?? det.flare_lon;
        const lat = det.lat ?? det.flare_lat;
        const gRow = Math.floor(lat / CELL_DEG);
        const gCol = Math.floor(lon / CELL_DEG);
        let bestIdx = -1, bestDist = Infinity;

        for (let dr = -1; dr <= 1; dr++) {
            for (let dc = -1; dc <= 1; dc++) {
                const key = (gRow + dr) * KEY_SHIFT + (gCol + dc);
                const bucket = grid.get(key);
                if (!bucket) continue;
                for (const ci of bucket) {
                    const a = clusterList[ci].anchor;
                    const aLon = a.lon ?? a.flare_lon;
                    const aLat = a.lat ?? a.flare_lat;
                    const d = fastDistM(lat, lon, aLat, aLon);
                    if (d <= mergeDistance && d < bestDist) {
                        bestDist = d;
                        bestIdx = ci;
                    }
                }
            }
        }

        if (bestIdx >= 0) {
            clusterList[bestIdx].members.push(det);
        } else {
            const ci = clusterList.length;
            clusterList.push({ anchor: det, members: [det] });
            const key = gRow * KEY_SHIFT + gCol;
            const bucket = grid.get(key);
            if (bucket) bucket.push(ci);
            else grid.set(key, [ci]);
        }
    }

    const result = [];
    for (const cluster of clusterList) {
        const members = cluster.members;

        // Deduplicate: keep best detection per date.
        const byDate = {};
        for (const d of members) {
            if (!byDate[d.date] || d.max_b12 > byDate[d.date].max_b12) byDate[d.date] = d;
        }
        const deduped = Object.values(byDate);

        if (deduped.length < minDates) continue;

        const avgB12 = deduped.reduce((s, d) => s + d.max_b12, 0) / deduped.length;
        if (avgB12 < minAvgB12) continue;

        // Anchor = highest B12 detection.
        let anchor = deduped[0];
        for (const d of deduped) { if (d.max_b12 > anchor.max_b12) anchor = d; }

        const anchorLon = anchor.lon ?? anchor.flare_lon;
        const anchorLat = anchor.lat ?? anchor.flare_lat;

        const dates = deduped.map(d => d.date).sort();
        const firstDate = dates[0];
        const lastDate = dates[dates.length - 1];

        const seasonal = isSeasonal(deduped);

        // Persistence calculation.
        let persistence = null;
        if (observations !== null) {
            let cloudFreeCount = 0;
            for (const [date, meta] of observations) {
                if (meta.cloudFree) cloudFreeCount++;
            }
            persistence = cloudFreeCount > 0 ? deduped.length / cloudFreeCount : null;
        }

        result.push({
            lon: anchorLon,
            lat: anchorLat,
            max_b12: anchor.max_b12,
            avg_b12: avgB12,
            detection_count: deduped.length,
            date_count: deduped.length,
            first_date: firstDate,
            last_date: lastDate,
            persistence,
            seasonal,
            detections: deduped.map(d => ({
                date: d.date,
                max_b12: d.max_b12,
                pixels: d.pixels,
                lon: d.lon ?? d.flare_lon,
                lat: d.lat ?? d.flare_lat,
            })),
        });
    }

    return result;
}
