// Public API barrel for the s2-flares methodology core. Pure ES modules; runs in
// the browser (vendored geotiff), Node/Bun/Deno (npm geotiff), Web Workers, and
// AWS Lambda. Consumers import only what they need — tree-shaking keeps the
// browser bundle to the detector + clustering.

// Detector: pure block-level computation + tunable thresholds.
export {
    detectBlock, DEFAULTS, LOOSE, resolveThresholds,
    BLOCK_SIZE, BLOCK_OVERLAP, dnToReflectance, screenClouds,
    glintAngleNadir, glintScoreFromAngle,
} from './detect.js';

// I/O: COG reads, block enumeration, whole-scene detection.
export { openCOG, readWindow, enumerateBlocks, detectImage } from './cog.js';

// Clear-sky coverage: the honest n_clear_obs denominator behind persistence.
export { coverImage } from './coverage.js';

// Cross-date spatial clustering (attaches the quality score per cluster).
export { clusterDetections } from './cluster.js';

// Scoring: the vision-validated quality model and glint geometry.
export {
    scoreCluster, ratioScore, persistenceScore, glintPenalty, glintSuspect,
    glintScoreFromElevation,
} from './score.js';

// STAC search (Element84 Earth-Search, adds sun angles for glint).
export { searchSTAC } from './stac.js';

// Projection helpers.
export { wgs84ToUtm, utmToWgs84, utmParams } from './geo.js';

import { searchSTAC } from './stac.js';
import { detectImage } from './cog.js';

// Primary streaming entry point: search STAC and process images, yielding
// progress + detection events. options.thresholds selects the preset (LOOSE for
// recall-first bulk runs); omit for the proven DEFAULTS.
export async function* detect(bbox, start, end, options = {}) {
    const { signal, skipDates, maxCloudCover, thresholds } = options;
    const skip = skipDates ? new Set(skipDates) : null;
    const items = [];

    for await (const item of searchSTAC(bbox, start, end, { signal, maxCloudCover })) {
        items.push(item);
    }

    const skipped = skip ? items.filter(it => skip.has(it.date)).length : 0;
    const observations = new Map();

    for (let i = 0; i < items.length; i++) {
        if (signal?.aborted) return;
        const item = items[i];

        if (skip?.has(item.date)) {
            yield { type: 'progress', imagesProcessed: i + 1, imagesTotal: items.length, imagesSkipped: skipped };
            continue;
        }

        yield { type: 'image-start', item, date: item.date, cloudCover: item.cloudCover };

        const result = await detectImage(item, bbox, { signal, thresholds });
        observations.set(item.date, { cloudFree: result.cloudFree });

        if (result.detections.length > 0) {
            yield { type: 'detections', features: result.detections, date: item.date, cloudFree: result.cloudFree };
        }

        yield { type: 'image-done', date: item.date, blocksProcessed: result.blocksProcessed };
        yield { type: 'progress', imagesProcessed: i + 1, imagesTotal: items.length, imagesSkipped: skipped };
    }
}
