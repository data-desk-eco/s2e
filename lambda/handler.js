// AWS Lambda entry point. One invocation = one S2 scene clipped to a bbox.
//
// The detector core lives in ../lib (shared with the browser, the CLI, and the
// Web Worker) — the Lambda is just an I/O shell around it, co-located with the
// public sentinel-2-l2a COG bucket in us-west-2 so byte-range reads are in-region.
//
// Detections are written as a per-scene CSV to S3 (one object per scene) rather
// than returned in the invoke response — under the recall-first LOOSE preset an
// interior MGRS tile produces far more than the 6 MB synchronous-response limit
// allows (Function.ResponseSizeTooLarge). The response carries only {key, count}
// so it stays tiny no matter how many detections a scene has. PutObject is atomic,
// so object presence == scene done, which makes the collector trivially resumable.
//
// Event:
//   {
//     item: {                      // normalised STAC item (lib/stac.js shape)
//       date, mgrs, id, epsg, sunElevation, sunAzimuth,
//       bands: { b12, b11, b8a, scl }   // COG hrefs
//     },
//     bbox: [west, south, east, north], // WGS84, the query window
//     mode: 'coverage' | undefined,     // 'coverage' = SCL clear-sky pass, no detection
//     sites: [{ h3, lon, lat }],        // coverage mode only
//     chunk: number,                    // coverage mode only (per-chunk CSV key)
//     screenOverview: false,            // optional, default false (max recall)
//     thresholds: { ...overrides },     // optional, merged over DEFAULTS; LOOSE for bulk
//     prefix: 's2',                     // optional, overrides S2_PREFIX
//   }
//
// Env: S2_BUCKET (required), S2_PREFIX (default "s2").
// Returns: { mgrs, date, scene, key, count, cloudFree, blocksProcessed, skippedOverview }

import { detectImage } from '../lib/cog.js';
import { coverImage } from '../lib/coverage.js';
import { resolveThresholds } from '../lib/detect.js';
// AWS SDK v3 is provided by the nodejs22.x runtime — not bundled, so the deploy
// zip stays tiny.
import { S3Client, PutObjectCommand } from '@aws-sdk/client-s3';

const s3 = new S3Client({});
// Bucket is env-only (the function's IAM policy is scoped to it). The prefix may
// be overridden per-invocation so the caller controls object layout in one place.
const BUCKET = process.env.S2_BUCKET;
const DEFAULT_PREFIX = process.env.S2_PREFIX || 's2';

// CSV column headers match the permian-flaring loader (sql/00_load.sql). The
// detector emits the B11 peak as `peak_b11` (main's field name); the loader's
// column is `max_b11`, so that one header reads a differently-named field.
const COLS = [
    'lon', 'lat', 'date', 'mgrs', 'scene',
    'max_b12', 'avg_b12', 'max_b11', 'b12_b11_ratio', 'peakedness',
    'pixels', 'warm_size', 'saturated',
    'sun_elevation', 'sun_azimuth', 'glint_angle', 'glint_score',
];
const FIELD = { max_b11: 'peak_b11' };
const csvRow = d => COLS.map(c => (d[FIELD[c] ?? c] ?? '')).join(',');

// SCL clear-sky coverage CSV: one row per (site, scene) with the 12-bin SCL class
// histogram. Same per-scene-CSV-to-S3 + atomic-PutObject-resume pattern as detection.
const COV_COLS = [
    'h3', 'date', 'mgrs', 'scene', 'sun_elevation', 'sun_azimuth', 'px_valid',
    ...Array.from({ length: 12 }, (_, i) => `scl${i}`),
];
const covRow = d => [
    d.h3, d.date, d.mgrs, d.scene, d.sun_elevation ?? '', d.sun_azimuth ?? '',
    d.px_valid, ...d.hist,
].join(',');

export async function handler(event) {
    const { item, bbox, sites, mode, screenOverview = false, thresholds, prefix } = event;
    if (!item || !bbox) throw new Error('missing required fields: item, bbox');
    if (!BUCKET) throw new Error('S2_BUCKET env not set');

    // Clear-sky coverage pass: sample the SCL band at each catalogue site, no detection.
    if (mode === 'coverage') {
        const rows = await coverImage(item, bbox, sites || []);
        // Dense tiles carry > 60k sites — past the 6 MB invoke-request cap — so the
        // collector splits a scene's sites into chunks; `chunk` makes the per-chunk
        // CSV key unique. The load globs them all back together.
        const suffix = event.chunk != null ? `_c${event.chunk}` : '';
        const key = `${prefix || DEFAULT_PREFIX}/${item.mgrs}_${item.date}${suffix}.csv`;
        const csv = [COV_COLS.join(','), ...rows.map(covRow)].join('\n') + '\n';
        await s3.send(new PutObjectCommand({
            Bucket: BUCKET, Key: key, Body: csv, ContentType: 'text/csv',
        }));
        return {
            mgrs: item.mgrs ?? null, date: item.date ?? null,
            scene: item.id ?? item.scene ?? null, key, count: rows.length,
        };
    }

    const T = resolveThresholds(thresholds);
    const result = await detectImage(item, bbox, { screenOverview, thresholds: T });
    const dets = result.detections;

    const key = `${prefix || DEFAULT_PREFIX}/${item.mgrs}_${item.date}.csv`;
    const csv = [COLS.join(','), ...dets.map(csvRow)].join('\n') + '\n';
    await s3.send(new PutObjectCommand({
        Bucket: BUCKET, Key: key, Body: csv, ContentType: 'text/csv',
    }));

    return {
        mgrs: item.mgrs ?? null,
        date: item.date ?? null,
        scene: item.id ?? item.scene ?? null,
        key,
        count: dets.length,
        cloudFree: result.cloudFree,
        blocksProcessed: result.blocksProcessed,
        skippedOverview: result.skippedOverview || false,
    };
}
