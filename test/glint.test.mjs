// Unit tests for the glint discriminator in clusterDetections.
// Run with: bun test/glint.test.mjs

import { clusterDetections } from '../lib/cluster.js';

let pass = 0, fail = 0;
function assert(name, cond, extra = '') {
    if (cond) { pass++; console.log(`  ok   ${name}`); }
    else      { fail++; console.log(`  FAIL ${name}${extra ? ' — ' + extra : ''}`); }
}

// Helper: build a synthetic detection
function det(date, max_b12, peak_b11, sun_elevation, lon = -102.0, lat = 32.0) {
    return { date, max_b12, peak_b11, sun_elevation, sun_azimuth: 150, pixels: 1, lon, lat };
}

console.log('\n[1] Glint cluster — saturating B12 ≈ B11, all summer high-sun detections');
{
    const dets = [];
    for (const m of ['05','06','07','08']) {
        for (const d of ['05','15','25']) {
            dets.push(det(`2024-${m}-${d}`, 1.41, 1.40, 65));
        }
    }
    const [c] = clusterDetections(dets, { minDates: 4, minAvgB12: 0.5 });
    assert('cluster created', !!c);
    assert('peak_b11 propagated', c.detections[0].peak_b11 === 1.40);
    assert('median_b12_b11_ratio close to 1.0',
        Math.abs(c.median_b12_b11_ratio - 1.41/1.40) < 1e-6,
        `got ${c.median_b12_b11_ratio}`);
    assert('min_sun_elevation = 65', c.min_sun_elevation === 65);
    assert('likely_glint = TRUE', c.likely_glint === true);
}

console.log('\n[2] Real flare — strong thermal ratio');
{
    const dets = [];
    for (const m of ['05','06','07','08']) {
        for (const d of ['05','15','25']) {
            // peak B12 0.9, peak B11 0.4 → ratio 2.25 (clearly thermal)
            dets.push(det(`2024-${m}-${d}`, 0.9, 0.4, 65));
        }
    }
    const [c] = clusterDetections(dets, { minDates: 4, minAvgB12: 0.5 });
    assert('cluster created', !!c);
    assert('high spectral ratio', c.median_b12_b11_ratio > 1.5);
    assert('likely_glint = FALSE (thermal)', c.likely_glint === false);
}

console.log('\n[3] Mega-flare that saturates both B12 and B11 — known soft-warning false positive');
{
    // Notes call this out: a steady-state flare large enough to saturate B11 too is
    // statistically indistinguishable from glint on the spectral ratio. Documented as
    // an accepted edge case — likely_glint is a soft warning, not a hard reject.
    const dets = [
        det('2024-01-15', 1.41, 1.40, 28),
        det('2024-02-15', 1.41, 1.40, 35),
        det('2024-06-15', 1.41, 1.40, 70),
        det('2024-07-15', 1.41, 1.40, 70),
    ];
    const [c] = clusterDetections(dets, { minDates: 4, minAvgB12: 0.5 });
    assert('cluster created', !!c);
    assert('min_sun_elevation = 28', c.min_sun_elevation === 28);
    assert('likely_glint = TRUE (accepted edge case)', c.likely_glint === true);
}

console.log('\n[4] Missing fields (legacy detections) — likely_glint = null');
{
    const dets = [
        { date: '2024-05-01', max_b12: 1.0, pixels: 1, lon: -102, lat: 32 },
        { date: '2024-05-11', max_b12: 1.0, pixels: 1, lon: -102, lat: 32 },
        { date: '2024-05-21', max_b12: 1.0, pixels: 1, lon: -102, lat: 32 },
        { date: '2024-05-31', max_b12: 1.0, pixels: 1, lon: -102, lat: 32 },
    ];
    const [c] = clusterDetections(dets, { minDates: 4, minAvgB12: 0.5 });
    assert('cluster created', !!c);
    assert('median_b12_b11_ratio = null', c.median_b12_b11_ratio === null);
    assert('min_sun_elevation = null', c.min_sun_elevation === null);
    assert('likely_glint = null', c.likely_glint === null);
}

console.log('\n[5] Borderline — ratio just above 1.25 threshold counts as thermal');
{
    const dets = [
        det('2024-05-15', 1.0, 0.78, 60),  // ratio ~1.282, just above 1.25
        det('2024-06-15', 1.0, 0.78, 65),
        det('2024-07-15', 1.0, 0.78, 70),
        det('2024-08-15', 1.0, 0.78, 65),
    ];
    const [c] = clusterDetections(dets, { minDates: 4, minAvgB12: 0.5 });
    assert('thermal ratio wins', c.likely_glint === false);
}

console.log('\n[6] Borderline — ratio just below 1.25 threshold flags as glint');
{
    const dets = [
        det('2024-05-15', 1.0, 0.83, 60),  // ratio ~1.205, below 1.25
        det('2024-06-15', 1.0, 0.83, 65),
        det('2024-07-15', 1.0, 0.83, 70),
        det('2024-08-15', 1.0, 0.83, 65),
    ];
    const [c] = clusterDetections(dets, { minDates: 4, minAvgB12: 0.5 });
    assert('flagged as glint', c.likely_glint === true);
}

console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail > 0 ? 1 : 0);
