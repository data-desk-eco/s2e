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
        const { bbox, start, end, clusterOptions, skipDates, priorDetections, maxCloudCover } = e.data;
        abortController = new AbortController();
        const allDetections = priorDetections ? [...priorDetections] : [];

        try {
            for await (const event of detect(bbox, start, end, { signal: abortController.signal, skipDates, maxCloudCover })) {
                if (event.type === 'detections') {
                    allDetections.push(...event.features);
                    self.postMessage({ type: 'detections', features: event.features, date: event.date });
                    // Incremental clustering after each new batch
                    const clusters = clusterDetections(allDetections, clusterOptions);
                    self.postMessage({ type: 'clusters', features: clusters });
                } else if (event.type === 'progress') {
                    self.postMessage({ type: 'progress', done: event.imagesProcessed, total: event.imagesTotal, skipped: event.imagesSkipped || 0 });
                } else if (event.type === 'image-done') {
                    self.postMessage({ type: 'image-done', date: event.date });
                }
            }

            // Final clustering (covers prior detections with no new detections)
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
