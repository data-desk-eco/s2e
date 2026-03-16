// Lambda handler for S2 flare detection — processes a single STAC item.
// Shares the exact same detection code as the web library and CLI.
//
// Input:  { item: <STAC item from searchSTAC()>, bbox: [W,S,E,N] }
// Output: { detections: [...], cloudFree: boolean, blocksProcessed: number, skippedOverview: boolean }

import { detectImage } from '../cog.js';

export async function handler(event) {
    const { item, bbox } = event;

    if (!item || !bbox) {
        throw new Error('Missing required fields: item, bbox');
    }

    const result = await detectImage(item, bbox);
    return {
        detections: result.detections,
        cloudFree: result.cloudFree,
        blocksProcessed: result.blocksProcessed,
        skippedOverview: result.skippedOverview || false,
    };
}
