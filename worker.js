import { detect } from './index.js';
import { clusterDetections } from './cluster.js';

let abortController = null;

self.onmessage = async (e) => {
    const { type } = e.data;

    if (type === 'cancel') {
        abortController?.abort();
        return;
    }

    if (type === 'detect') {
        const { bbox, start, end, clusterOptions } = e.data;
        abortController = new AbortController();
        const allDetections = [];

        try {
            for await (const event of detect(bbox, start, end, { signal: abortController.signal })) {
                if (event.type === 'detections') {
                    allDetections.push(...event.features);
                    self.postMessage({ type: 'detections', features: event.features, date: event.date });
                } else if (event.type === 'progress') {
                    self.postMessage({ type: 'progress', done: event.imagesProcessed, total: event.imagesTotal });
                }
            }

            // Run clustering — consumer can pass threshold overrides
            const clusters = clusterDetections(allDetections, clusterOptions);
            self.postMessage({ type: 'clusters', features: clusters });
            self.postMessage({ type: 'done' });
        } catch (err) {
            if (err.name !== 'AbortError') {
                self.postMessage({ type: 'error', message: err.message });
            }
        }
    }
};
