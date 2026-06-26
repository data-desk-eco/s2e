// per-scene detection cache — the read-through layer behind the web API.
//
// the cache atom is a whole sentinel-2 scene: one (preset, mgrs tile, date). a
// flare at (lon,lat) on a date is a fact independent of the viewport that asked,
// so we cache the *detections* (the expensive COG-read step), not the viewport
// query and not the clusters (clustering is a cheap pure function run on read).
//
// on a miss we detect the WHOLE tile (item.bbox), not just the requested
// viewport — that is what makes "object present == this tile@date is done" honest
// and what grows a complete, duckdb-queryable collection one tile at a time. the
// first viewport into a cold tile pays; every later viewport in it reads for free.
//
// layout: <prefix>/<preset>/tile=<mgrs>/<date>.parquet  (hive-partitioned on
// tile + self-describing columns, so `read_parquet('s3://…/**/*.parquet',
// hive_partitioning=true)` is the whole analytics story). preset namespaces the
// key because thresholds change the detections, so a LOOSE tile must never be
// served to a DEFAULT request.
//
// cloud-free status — the persistence denominator, needed even for a scene with
// zero detections — rides in object metadata, so an empty scene is still a
// complete cache entry (a valid zero-row parquet + its cloudFree flag).

import { S3Client, GetObjectCommand, PutObjectCommand } from '@aws-sdk/client-s3';
import { parquetWriteBuffer } from 'hyparquet-writer';
import { parquetReadObjects } from 'hyparquet';
import { detectImage } from '../lib/cog.js';

const s3 = new S3Client({});

// detection fields persisted per row, in the detector's own field names so a
// cache read feeds clusterDetections directly (no renaming) and the columns are
// self-describing for duckdb. explicit types so a zero-row scene still writes a
// valid, typed parquet.
const COLS = {
    lon: 'DOUBLE', lat: 'DOUBLE', date: 'STRING', mgrs: 'STRING', scene: 'STRING',
    max_b12: 'DOUBLE', avg_b12: 'DOUBLE', peak_b11: 'DOUBLE', b12_b11_ratio: 'DOUBLE',
    peakedness: 'DOUBLE', pixels: 'INT32', warm_size: 'INT32', saturated: 'BOOLEAN',
    sun_elevation: 'DOUBLE', sun_azimuth: 'DOUBLE', glint_angle: 'DOUBLE', glint_score: 'DOUBLE',
};
const NAMES = Object.keys(COLS);

const encode = dets => new Uint8Array(parquetWriteBuffer({
    columnData: NAMES.map(name => ({ name, type: COLS[name], data: dets.map(d => d[name] ?? null) })),
}));

const is404 = e => e?.name === 'NoSuchKey' || e?.$metadata?.httpStatusCode === 404;

/**
 * Build a scene store closure for runAOI's `store` hook.
 * @returns {(item, signal?) => Promise<{detections, cloudFree}>} whole-tile
 *   detections for one scene, served from S3 or computed+cached on miss.
 */
export function makeSceneStore({ bucket, prefix = 'flares', preset = 'default', thresholds }) {
    if (!bucket) throw new Error('makeSceneStore: bucket required');
    const keyOf = item => `${prefix}/${preset}/tile=${item.mgrs}/${item.date}.parquet`;

    return async function store(item, signal) {
        const Key = keyOf(item);
        try {
            const got = await s3.send(new GetObjectCommand({ Bucket: bucket, Key }));
            const u8 = await got.Body.transformToByteArray();
            const detections = await parquetReadObjects({
                file: u8.buffer.slice(u8.byteOffset, u8.byteOffset + u8.byteLength),
            });
            return { detections, cloudFree: got.Metadata?.cloudfree === '1' };
        } catch (e) {
            if (!is404(e)) throw e;
        }
        // miss — detect the whole tile, cache it (atomic PutObject == done), return.
        const r = await detectImage(item, item.bbox, { thresholds, signal, screenOverview: true });
        await s3.send(new PutObjectCommand({
            Bucket: bucket, Key, Body: encode(r.detections),
            ContentType: 'application/vnd.apache.parquet',
            Metadata: { cloudfree: r.cloudFree ? '1' : '0' },
        }));
        return { detections: r.detections, cloudFree: r.cloudFree };
    };
}
