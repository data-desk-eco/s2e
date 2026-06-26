// Fan the s2-flares detection Lambda out over a set of areas of interest derived
// from a Global Energy Monitor terminals geojson — here, every global LNG EXPORT
// terminal. Generalises permian-flaring's single-bbox collect_s2.mjs to many AOIs.
//
// Train dedup: GEM lists each liquefaction train/unit as its own feature, but all
// units of one terminal share a ProjectID. We group by ProjectID and scan the
// padded ENVELOPE of a terminal's units once — so an N-train terminal is one AOI,
// not N overlapping boxes. Per-AOI S3 prefix (`<prefix>/<ProjectID>`) keeps scenes
// from terminals that share an MGRS tile from colliding on the same object key.
//
// Recall-first LOOSE preset (quality gating is downstream, permian-style). The
// Lambda writes one CSV per scene to S3; PutObject is atomic, so object presence
// == scene done → resumable. Set DRY_RUN=1 to report the plan without invoking.

import { readFileSync } from 'node:fs';
import { Agent } from 'node:https';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { LambdaClient, InvokeCommand } from '@aws-sdk/client-lambda';
import { NodeHttpHandler } from '@smithy/node-http-handler';
import { searchSTAC } from '../lib/stac.js';
import { LOOSE } from '../lib/detect.js';

const pexec = promisify(execFile);

const FN = process.env.FUNCTION_NAME, REGION = process.env.REGION || 'us-west-2';
const BUCKET = process.env.S3_BUCKET, PREFIX = process.env.S3_PREFIX || 'lng';
const START = process.env.START, END = process.env.END;
const PAD = Number(process.env.PAD || 0.03);            // ~3 km terminal halo
const CONC = Number(process.env.S2_CONCURRENCY || 16);
const LIMIT = Number(process.env.LIMIT || 0);           // cap AOIs (testing)
const DRY = process.env.DRY_RUN === '1';
const keep = (process.env.STATUS || 'operating,construction,idled,mothballed,retired');
const STATUS = keep === 'all' ? null : new Set(keep.split(','));

// --- AOIs: export terminals, one padded envelope per GEM ProjectID -----------
const gj = JSON.parse(readFileSync(process.env.AOI_GEOJSON, 'utf8'));
const groups = new Map();
for (const { properties: p } of gj.features) {
    if (p.FacilityType !== 'export') continue;
    if (STATUS && !STATUS.has(p.Status)) continue;
    if (p.Longitude == null || p.Latitude == null) continue;
    let g = groups.get(p.ProjectID);
    if (!g) groups.set(p.ProjectID, g = { id: p.ProjectID, name: p.TerminalName, lon: [], lat: [] });
    g.lon.push(p.Longitude); g.lat.push(p.Latitude);
}
let aois = [...groups.values()].map(g => ({
    id: g.id, name: g.name,
    bbox: [Math.min(...g.lon) - PAD, Math.min(...g.lat) - PAD,
           Math.max(...g.lon) + PAD, Math.max(...g.lat) + PAD],
}));
if (LIMIT) aois = aois.slice(0, LIMIT);
console.error(`${aois.length} export terminals (status=${keep}) · ${START}..${END} · preset=loose`);

const lambda = new LambdaClient({
    region: REGION, maxAttempts: 10, retryMode: 'adaptive',
    requestHandler: new NodeHttpHandler({
        httpsAgent: new Agent({ keepAlive: true, maxSockets: CONC + 50 }),
    }),
});

// Already-present scene CSVs (resume). aws CLI auto-paginates; 'None' == empty.
async function listDone() {
    if (DRY) return new Set();
    const { stdout } = await pexec('aws', ['s3api', 'list-objects-v2',
        '--bucket', BUCKET, '--prefix', `${PREFIX}/`, '--region', REGION,
        '--query', 'Contents[].Key', '--output', 'text'],
        { maxBuffer: 512 * 1024 * 1024 });
    return new Set(stdout.split(/\s+/).filter(k => k && k !== 'None'));
}

async function invoke(aoi, item) {
    const payload = { item, bbox: aoi.bbox, screenOverview: false,
        thresholds: LOOSE, prefix: `${PREFIX}/${aoi.id}` };
    const r = await lambda.send(new InvokeCommand({
        FunctionName: FN, Payload: Buffer.from(JSON.stringify(payload)),
    }));
    const body = JSON.parse(Buffer.from(r.Payload).toString());
    if (r.FunctionError) throw new Error(body.errorMessage || r.FunctionError);
    return body;
}

// --- Phase 1: enumerate scenes per AOI, drop ones already in S3 --------------
const done = await listDone();
const tasks = [];
let n = 0;
for (const aoi of aois) {
    for await (const item of searchSTAC(aoi.bbox, START, END)) {
        const key = `${PREFIX}/${aoi.id}/${item.mgrs}_${item.date}.csv`;
        if (!done.has(key)) tasks.push({ aoi, item });
    }
    if (++n % 25 === 0) console.error(`  searched ${n}/${aois.length} terminals · ${tasks.length} scenes queued`);
}
console.error(`${tasks.length} scenes to do · ${done.size} already in s3`);
if (DRY) {
    for (const { aoi } of tasks.slice(0, 5)) console.error(`  e.g. ${aoi.id} ${aoi.name} [${aoi.bbox.map(x => x.toFixed(3))}]`);
    process.exit(0);
}

// --- Phase 2: fan out, CONC invocations in flight ----------------------------
let i = 0, ok = 0, dets = 0, failed = 0;
async function worker() {
    while (i < tasks.length) {
        const { aoi, item } = tasks[i++];
        try {
            const body = await invoke(aoi, item);
            ok++; dets += body.count || 0;
            if (ok % 50 === 0) console.error(`  ${ok}/${tasks.length} scenes · ${dets} detections`);
        } catch (err) {
            failed++;
            console.error(`  FAIL ${aoi.id} ${item.mgrs}_${item.date}: ${err.message}`);
        }
    }
}
await Promise.all(Array.from({ length: Math.min(CONC, tasks.length) }, worker));
console.error(`done: ${ok}/${tasks.length} scenes, ${dets} detections, ${failed} failed`);
console.error(`results: s3://${BUCKET}/${PREFIX}/  (sync: aws s3 sync s3://${BUCKET}/${PREFIX}/ data/lng/ --region ${REGION})`);
