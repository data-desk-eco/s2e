// End-to-end test: run detection over the bbox of two known clusters
// and verify the new sun-elevation/B11 discriminator correctly tags
// glint vs real flare.
//
//   1d14fzj — confirmed sun glint off oil tank tops (false positive)
//   b6g3x0  — confirmed real flare on a Diamondback well pad
//
// Run with: bun test/glint-real.test.mjs

import { detect } from '../lib/index.js';
import { clusterDetections } from '../lib/cluster.js';

const TARGETS = [
    { id: '1d14fzj', label: 'tank glint',     lon: -102.0208, lat: 32.0071, expectGlint: true  },
    { id: 'b6g3x0',  label: 'real flare',     lon: -101.8831, lat: 32.1935, expectGlint: false },
];

// Sample a window that includes both summer (high sun) and winter (low sun)
// observations — needed to test the sun-elevation tie-breaker.
const START = '2024-06-01';
const END   = '2025-02-15';

const HALF_M = 375;

async function runOne(target) {
    const dLat = HALF_M / 110540;
    const dLon = HALF_M / (111320 * Math.cos(target.lat * Math.PI / 180));
    const bbox = [target.lon - dLon, target.lat - dLat, target.lon + dLon, target.lat + dLat];

    const dets = [];
    process.stderr.write(`\n[${target.label} ${target.id}] bbox=${bbox.map(x => x.toFixed(4)).join(',')}\n`);

    let processed = 0, total = 0;
    for await (const event of detect(bbox, START, END, { maxCloudCover: 50 })) {
        if (event.type === 'detections') {
            dets.push(...event.features);
        } else if (event.type === 'progress') {
            processed = event.imagesProcessed; total = event.imagesTotal;
            process.stderr.write(`\r  ${processed}/${total} scenes, ${dets.length} detections   `);
        }
    }
    process.stderr.write('\n');

    const clusters = clusterDetections(dets, { minDates: 1, minAvgB12: 0.5 });
    return { target, dets, clusters };
}

let pass = 0, fail = 0;
function assert(name, cond, extra = '') {
    if (cond) { pass++; console.log(`  ok   ${name}`); }
    else      { fail++; console.log(`  FAIL ${name}${extra ? ' — ' + extra : ''}`); }
}

// Run both targets concurrently — they hit independent S2 tiles.
const allResults = await Promise.all(TARGETS.map(runOne));
for (const { target: t, dets, clusters } of allResults) {
    console.log(`\n  ${dets.length} raw detections, ${clusters.length} clusters`);
    if (clusters.length === 0) {
        fail++;
        console.log(`  FAIL no cluster found for ${t.id}`);
        continue;
    }
    // Pick the cluster nearest the target.
    let best = clusters[0], bestD = Infinity;
    for (const c of clusters) {
        const d = Math.hypot(c.lon - t.lon, c.lat - t.lat);
        if (d < bestD) { bestD = d; best = c; }
    }
    console.log(`  cluster ${best.id} @ ${best.lat.toFixed(4)},${best.lon.toFixed(4)}`);
    console.log(`    detections        : ${best.detection_count}`);
    console.log(`    max_b12 / avg_b12 : ${best.max_b12.toFixed(3)} / ${best.avg_b12.toFixed(3)}`);
    console.log(`    median b12/b11    : ${best.median_b12_b11_ratio?.toFixed(3) ?? '—'}`);
    console.log(`    min sun elevation : ${best.min_sun_elevation?.toFixed(1) ?? '—'}°`);
    console.log(`    likely_glint      : ${best.likely_glint}`);

    // Print first few detections for inspection
    for (const d of best.detections.slice(0, 4)) {
        console.log(`      ${d.date}  b12=${d.max_b12.toFixed(3)} b11=${d.peak_b11?.toFixed(3) ?? '—'} sun=${d.sun_elevation?.toFixed(1) ?? '—'}°`);
    }

    assert(`[${t.label}] peak_b11 captured`, best.detections[0].peak_b11 != null);
    assert(`[${t.label}] sun_elevation captured`, best.detections[0].sun_elevation != null);
    assert(`[${t.label}] likely_glint = ${t.expectGlint}`, best.likely_glint === t.expectGlint,
        `got ${best.likely_glint}`);
}

console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail > 0 ? 1 : 0);
