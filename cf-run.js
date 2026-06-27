#!/usr/bin/env node

// EU-sovereign bulk runner — lambda/handler.js's per-scene fan-out, but detection
// runs locally on the CloudFerro box via the gdal JP2 reader (lib/cog-gdal.js)
// over the co-located `eodata` archive, not on AWS Lambda over the public COGs.
// Search is repointed at CDSE. Output is one CSV per scene under
// <out>/<aoi>/<mgrs>_<date>.csv with the handler's columns; file presence ==
// scene done, so runs are resumable and the box scales to zero between them.
//
// node (not bun): gdal-async is a node native addon, so this is a node sibling of
// the bun cli.js — same lib/ core, node fs instead of Bun.* and a local worker
// pool instead of LambdaClient.fan-out.
//
// Usage: node cf-run.js (--bbox W,S,E,N | --aoi file.geojson) --out DIR [options]

import { mkdirSync, existsSync, writeFileSync, readFileSync } from 'node:fs';
import { dirname } from 'node:path';
import { searchSTAC } from './lib/stac.js';
import { detectImage } from './lib/cog-gdal.js';
import { LOOSE, resolveThresholds } from './lib/detect.js';
import { geomBbox, padBbox } from './lib/geo.js';

// per-scene CSV schema — identical to lambda/handler.js (permian-flaring loader).
const COLS = ['lon', 'lat', 'date', 'mgrs', 'scene', 'max_b12', 'avg_b12', 'max_b11',
    'b12_b11_ratio', 'peakedness', 'pixels', 'warm_size', 'saturated',
    'sun_elevation', 'sun_azimuth', 'glint_angle', 'glint_score'];
const FIELD = { max_b11: 'peak_b11' };
const csvRow = d => COLS.map(c => (d[FIELD[c] ?? c] ?? '')).join(',');

function parseArgs(argv) {
    const a = { source: 'cdse', preset: 'loose', buffer: 0, concurrency: 4, maxCloudCover: 100, out: 'out' };
    for (let i = 2; i < argv.length; i++) {
        const k = argv[i], v = argv[i + 1];
        const set = (key, val) => { a[key] = val; i++; };
        if (k === '--bbox') set('bbox', v.split(',').map(Number));
        else if (k === '--aoi') set('aoi', v);
        else if (k === '--out') set('out', v);
        else if (k === '--start') set('start', v);
        else if (k === '--end') set('end', v);
        else if (k === '--buffer') set('buffer', Number(v));
        else if (k === '--cloud') set('maxCloudCover', Number(v));
        else if (k === '--preset') set('preset', v);
        else if (k === '--source') set('source', v);            // cdse (box) | aws (test)
        else if (k === '--concurrency') set('concurrency', Number(v));
        else { console.error(`unknown arg: ${k}`); process.exit(1); }
    }
    if (!a.bbox && !a.aoi) { console.error('provide --bbox W,S,E,N or --aoi file.geojson'); process.exit(1); }
    if (!a.start || !a.end) {
        const now = new Date(), ago = new Date(now); ago.setMonth(ago.getMonth() - 6);
        a.start ||= ago.toISOString().slice(0, 10); a.end ||= now.toISOString().slice(0, 10);
    }
    return a;
}

function loadAOIs(a) {
    if (a.bbox) return [{ id: 'aoi', name: '', bbox: a.bbox }];
    const gj = JSON.parse(readFileSync(a.aoi, 'utf8'));
    return (gj.features || []).map((f, i) => ({
        id: f.properties?.id ?? f.properties?.ProjectID ?? String(i),
        name: f.properties?.name ?? f.properties?.TerminalName ?? '',
        bbox: padBbox(geomBbox(f.geometry), a.buffer),
    }));
}

async function runAOIBulk(aoi, a, T) {
    const items = [];
    for await (const it of searchSTAC(aoi.bbox, a.start, a.end, { maxCloudCover: a.maxCloudCover, source: a.source })) items.push(it);
    let done = 0, detected = 0, skipped = 0, idx = 0;
    async function worker() {
        while (idx < items.length) {
            const item = items[idx++];
            const path = `${a.out}/${aoi.id}/${item.mgrs}_${item.date}.csv`;
            if (existsSync(path)) { skipped++; continue; }     // presence == done → resume
            try {
                const r = await detectImage(item, aoi.bbox, { thresholds: T, screenOverview: false,
                    harmonize: a.source !== 'aws' });   // aws COGs are pre-harmonised; eodata JP2 isn't
                mkdirSync(dirname(path), { recursive: true });
                writeFileSync(path, [COLS.join(','), ...r.detections.map(csvRow)].join('\n') + '\n');
                done++; detected += r.detections.length;
                process.stderr.write(`  [${done + skipped}/${items.length}] ${aoi.id} ${item.mgrs}_${item.date}: ${r.detections.length} det\n`);
            } catch (err) {
                process.stderr.write(`  ${aoi.id} ${item.mgrs}_${item.date} FAIL: ${err.message}\n`);
            }
        }
    }
    await Promise.all(Array.from({ length: Math.min(a.concurrency, items.length) || 1 }, worker));
    process.stderr.write(`  ${aoi.id} ${aoi.name}: ${items.length} scenes (${done} new, ${skipped} cached), ${detected} detections\n`);
    return { scenes: items.length, done, detected };
}

async function main() {
    const a = parseArgs(process.argv);
    const T = a.preset === 'loose' ? resolveThresholds(LOOSE) : resolveThresholds();
    const aois = loadAOIs(a);
    const t0 = Date.now();
    process.stderr.write(`cf-run: ${aois.length} AOI(s) | ${a.start} → ${a.end} | preset=${a.preset} | source=${a.source} → ${a.out}/\n`);
    let scenes = 0, detected = 0;
    for (const aoi of aois) { const r = await runAOIBulk(aoi, a, T); scenes += r.scenes; detected += r.detected; }
    process.stderr.write(`\ndone: ${scenes} scenes, ${detected} detections → ${a.out}/ (${((Date.now() - t0) / 1000).toFixed(1)}s)\n`);
    process.stderr.write(`query: duckdb -c "SELECT * FROM read_csv('${a.out}/**/*.csv', union_by_name=true)"\n`);
}

main().catch(err => { console.error('fatal:', err.message); process.exit(1); });
