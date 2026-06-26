// Whole-AOI pipeline: STAC search → concurrent per-scene detect → cross-date
// cluster. The one orchestration both the CLI (local mode) and the Lambda web
// API (buffered + streaming) call, so the search/detect/cluster wiring lives in
// exactly one place. The per-scene Lambda shell (lambda/handler.js) stays at its
// own granularity for bulk fan-out; this is the granularity a caller who has a
// geometry + dates actually wants.
//
// onEvent (optional) is called as work progresses, which is what makes streaming
// trivial — the API just serialises each event to the response:
//   { type: 'start',  scenes }                              search done
//   { type: 'scene',  date, mgrs, cloudFree, count, done, scenes }   one scene detected
//   { type: 'scene-error', date, mgrs, error, done, scenes }
//   { type: 'clusters', clusters }                          final, after clustering
// Detections stream per-scene but clustering is cross-date, so the map features
// only exist in the terminal 'clusters' event.

import { searchSTAC } from './stac.js';
import { detectImage } from './cog.js';
import { clusterDetections } from './cluster.js';

export async function runAOI(bbox, start, end, opts = {}) {
    const { thresholds, concurrency = 4, maxCloudCover = 100, signal,
            cluster = {}, onEvent } = opts;

    const items = [];
    for await (const it of searchSTAC(bbox, start, end, { maxCloudCover, signal })) items.push(it);
    onEvent?.({ type: 'start', scenes: items.length });

    const detections = [], observations = new Map();
    let idx = 0, done = 0;
    async function worker() {
        while (idx < items.length) {
            if (signal?.aborted) return;
            const item = items[idx++];
            try {
                const r = await detectImage(item, bbox, { thresholds, signal });
                observations.set(item.date, { cloudFree: r.cloudFree });
                for (const d of r.detections) detections.push(d);
                onEvent?.({ type: 'scene', date: item.date, mgrs: item.mgrs,
                    cloudFree: r.cloudFree, count: r.detections.length,
                    done: ++done, scenes: items.length });
            } catch (err) {
                onEvent?.({ type: 'scene-error', date: item.date, mgrs: item.mgrs,
                    error: err.message, done: ++done, scenes: items.length });
            }
        }
    }
    await Promise.all(Array.from({ length: Math.min(concurrency, items.length) || 1 }, worker));

    const clusters = clusterDetections(detections, { observations, ...cluster });
    onEvent?.({ type: 'clusters', clusters });
    return { clusters, observations, scenes: items.length };
}
