export { detectBlock } from './detect.js';
export { clusterDetections } from './cluster.js';
export { searchSTAC } from './stac.js';
export { detectImage } from './cog.js';

import { searchSTAC } from './stac.js';
import { detectImage } from './cog.js';

// Primary entry point: async generator that searches STAC and processes images
export async function* detect(bbox, start, end, options = {}) {
    const { signal } = options;
    const items = [];

    // Collect all STAC items first (need total for progress)
    for await (const item of searchSTAC(bbox, start, end, { signal })) {
        items.push(item);
    }

    const observations = new Map();

    for (let i = 0; i < items.length; i++) {
        if (signal?.aborted) return;
        const item = items[i];

        yield { type: 'image-start', item, date: item.date, cloudCover: item.cloudCover };

        const result = await detectImage(item, bbox, { signal });
        observations.set(item.date, { cloudFree: result.cloudFree });

        if (result.detections.length > 0) {
            yield { type: 'detections', features: result.detections, date: item.date, cloudFree: result.cloudFree };
        }

        yield { type: 'image-done', date: item.date, blocksProcessed: result.blocksProcessed };
        yield { type: 'progress', imagesProcessed: i + 1, imagesTotal: items.length };
    }
}
