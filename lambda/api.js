// s2-flares web API — a Lambda Function URL around the whole-AOI pipeline
// (lib/run.js). One request: a geometry (or bbox) + a date range → clustered,
// scored flare detections as GeoJSON. The natural client is an interactive map:
// the AOI is a viewport, panned and zoomed, so requests are small areas and the
// response is clusters (a handful of map pins), not raw pixels.
//
// A hard area cap (MAX_AOI_KM2) rejects oversized AOIs before any COG read, which
// keeps a public endpoint cheap and bounds the work an attacker can ask for; pair
// it with reserved concurrency at deploy time (lambda/deploy.sh PUBLIC_URL=1).
//
// Deployed with the RESPONSE_STREAM invoke mode, so one handler serves both:
//   - default            one JSON FeatureCollection, written when detection ends.
//   - ?stream=1          newline-delimited JSON events (start / scene / clusters)
//     (or Accept:        as each scene finishes — live pins + a progress bar on a
//      text/event-stream) panning map. Clustering is cross-date, so map features
//                        arrive only in the terminal `clusters` event.
//
// Request fields (query string for GET, or JSON body for POST; body wins):
//   geometry  GeoJSON geometry            | bbox  "W,S,E,N" or [W,S,E,N]
//   start/end YYYY-MM-DD (default: last DEFAULT_DAYS)
//   buffer    km halo around the AOI (default 0)
//   preset    default | loose             | stream  1 to stream
//
// Env: MAX_AOI_KM2 (default 2500), DEFAULT_DAYS (default 90).

import { runAOI } from '../lib/run.js';
import { LOOSE, resolveThresholds } from '../lib/detect.js';
import { geomBbox, padBbox, bboxAreaKm2 } from '../lib/geo.js';

const MAX_AOI_KM2 = Number(process.env.MAX_AOI_KM2 || 2500);
const DEFAULT_DAYS = Number(process.env.DEFAULT_DAYS || 90);
const ymd = d => d.toISOString().slice(0, 10);

// A cluster → a GeoJSON point Feature. The per-date `detections` array is dropped
// from the map view; the score + glint fields are what a pin needs.
const feature = ({ lon, lat, detections, ...props }) => ({
    type: 'Feature', id: props.id,
    geometry: { type: 'Point', coordinates: [lon, lat] }, properties: props,
});

// Merge query string under JSON body (body wins) and normalise to a request.
function parse(event) {
    const q = event.queryStringParameters || {};
    let body = {};
    if (event.body) {
        const raw = event.isBase64Encoded ? Buffer.from(event.body, 'base64').toString() : event.body;
        try { body = JSON.parse(raw); } catch { /* query-only request */ }
    }
    const s = { ...q, ...body };
    const bbox = s.geometry ? geomBbox(s.geometry)
        : s.bbox ? (Array.isArray(s.bbox) ? s.bbox : s.bbox.split(',')).map(Number)
        : null;
    const now = new Date();
    return {
        bbox: bbox && padBbox(bbox, Number(s.buffer) || 0),
        start: s.start || ymd(new Date(now - DEFAULT_DAYS * 864e5)),
        end: s.end || ymd(now),
        thresholds: s.preset === 'loose' ? resolveThresholds(LOOSE) : undefined,
        // Map-friendly clustering: surface every scored detection (minDates 1) and
        // let the client filter by the score slider; the two knobs that matter are
        // overridable. Bulk-research gating (minDates 4) is the CLI's job, not this.
        cluster: { minDates: Number(s.minDates) || 1, minAvgB12: 0.5,
            scoreThreshold: Number(s.minScore) || 0 },
        stream: s.stream === '1' || s.stream === true ||
            (event.headers?.accept || '').includes('text/event-stream'),
    };
}

const CORS = { 'access-control-allow-origin': '*' };

export const handler = awslambda.streamifyResponse(async (event, raw) => {
    const reply = (code, type) => awslambda.HttpResponseStream.from(raw,
        { statusCode: code, headers: { 'content-type': type, ...CORS } });
    const fail = (code, error) => { const s = reply(code, 'application/json'); s.write(JSON.stringify({ error })); s.end(); };

    const req = parse(event);
    if (!req.bbox || req.bbox.length !== 4 || req.bbox.some(n => !isFinite(n)))
        return fail(400, 'provide a GeoJSON `geometry` or `bbox=W,S,E,N`');
    const area = bboxAreaKm2(req.bbox);
    if (area > MAX_AOI_KM2)
        return fail(413, `AOI ${Math.round(area)} km² exceeds the ${MAX_AOI_KM2} km² cap — zoom in`);

    const { bbox, start, end, thresholds, cluster } = req;
    if (req.stream) {
        const s = reply(200, 'application/x-ndjson');
        await runAOI(bbox, start, end, { thresholds, cluster, onEvent: ev => s.write(JSON.stringify(
            ev.type === 'clusters' ? { type: 'clusters', features: ev.clusters.map(feature) } : ev) + '\n') });
        s.end();
        return;
    }
    const { clusters, scenes } = await runAOI(bbox, start, end, { thresholds, cluster });
    const s = reply(200, 'application/json');
    s.write(JSON.stringify({ type: 'FeatureCollection', scenes, features: clusters.map(feature) }));
    s.end();
});
