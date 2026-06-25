// Unit tests for the vision-validated cluster score (lib/score.js) and its
// attachment in clusterDetections. Run with: node test/score.test.mjs (or bun).

import {
    ratioScore, persistenceScore, glintPenalty, scoreCluster,
    glintScoreFromElevation, RATIO_FLOOR, RATIO_SPAN, W_RATIO, W_PERSIST, W_GLINT,
} from '../lib/score.js';
import { DEFAULTS } from '../lib/detect.js';
import { clusterDetections } from '../lib/cluster.js';

let pass = 0, fail = 0;
const approx = (a, b, eps = 1e-9) => Math.abs(a - b) < eps;
function assert(name, cond, extra = '') {
    if (cond) { pass++; console.log(`  ok   ${name}`); }
    else      { fail++; console.log(`  FAIL ${name}${extra ? ' — ' + extra : ''}`); }
}

console.log('\n[1] ratioScore — smooth ramp over [1.1, 1.7]');
assert('floor → 0', ratioScore(RATIO_FLOOR) === 0);
assert('below floor clamps to 0', ratioScore(0.5) === 0);
assert('midpoint 1.4 → 0.5', approx(ratioScore(RATIO_FLOOR + RATIO_SPAN / 2), 0.5));
assert('top 1.7 → 1', approx(ratioScore(RATIO_FLOOR + RATIO_SPAN), 1));
assert('above top clamps to 1', ratioScore(3.0) === 1);
assert('null → 0', ratioScore(null) === 0);

console.log('\n[2] persistenceScore — clear-sky share nDates/nObs');
assert('half lit', approx(persistenceScore(5, 10), 0.5));
assert('fully lit clamps to 1', persistenceScore(20, 10) === 1);
assert('no observations → 0', persistenceScore(3, 0) === 0);

console.log('\n[3] glintPenalty — linear in min glint, normalised −0');
assert('minGlint 1 → −W_GLINT', approx(glintPenalty(1), -W_GLINT));
assert('minGlint 0 → 0 (not −0)', Object.is(glintPenalty(0), 0));
assert('minGlint 0.5 → −0.2', approx(glintPenalty(0.5), -0.2));
assert('null → 0', glintPenalty(null) === 0);

console.log('\n[4] scoreCluster — weighted total, max is 0.90');
{
    const best = scoreCluster({ maxRatio: 2.0, nDates: 10, nObs: 10, minGlint: 0 });
    assert('ratio_score saturates', best.ratio_score === 1);
    assert('persistence_score saturates', best.persistence_score === 1);
    assert('max total_score = 0.90', approx(best.total_score, W_RATIO + W_PERSIST));
    assert('total never reaches 1.0', best.total_score < 1.0);

    // ratio at floor, no clear-sky budget (persistence 0), full glint → pure penalty.
    const glinty = scoreCluster({ maxRatio: 1.1, nDates: 1, nObs: 0, minGlint: 1 });
    assert('floor case = −W_GLINT', approx(glinty.total_score, -W_GLINT));

    // persistence still scores at ratio 0 via the 0.1 floor.
    const dim = scoreCluster({ maxRatio: 1.1, nDates: 10, nObs: 10, minGlint: 0 });
    assert('dim-but-persistent > 0', approx(dim.total_score, W_PERSIST * 0.1));
}

console.log('\n[5] glintScoreFromElevation — geometry');
assert('high sun (low glint angle) → 1', glintScoreFromElevation(80) === 1); // angle 10 ≤ 25
assert('low sun (high glint angle) → 0', glintScoreFromElevation(10) === 0); // angle 80 ≥ 65
assert('null elevation → null', glintScoreFromElevation(null) === null);

console.log('\n[6] DEFAULTS reproduce the legacy detector constants exactly');
{
    const legacy = {
        b12Min: 0.30, b11Min: 0.20, peakB12Min: 0.50, contrastRatio: 3.0,
        backgroundFloor: 0.15, peakednessMin: 1.15, saturation: 1.0, maxPixels: 80,
        largePixels: 30, largeB12Min: 0.70, warmFraction: 0.5, warmMaxPixels: 100,
        singlePixelMin: 0.65, maxCloudLocal: 0.75, cloudFreeThresh: 0.30,
    };
    let same = true;
    for (const k of Object.keys(legacy)) if (DEFAULTS[k] !== legacy[k]) { same = false; }
    assert('DEFAULTS == legacy constants', same);
}

console.log('\n[7] clusterDetections attaches the score components');
{
    const dets = [];
    const obs = new Map();
    for (const m of ['05', '06', '07', '08']) {
        for (const d of ['05', '15', '25']) {
            const date = `2024-${m}-${d}`;
            // strong thermal ratio (2.25), high sun → low glint
            dets.push({ date, max_b12: 0.9, peak_b11: 0.4, b12_b11_ratio: 2.25,
                sun_elevation: 70, sun_azimuth: 150, pixels: 1, lon: -102, lat: 32 });
            obs.set(date, { cloudFree: true });
        }
    }
    const [c] = clusterDetections(dets, { minDates: 4, minAvgB12: 0.5, observations: obs });
    assert('cluster created', !!c);
    assert('ratio_score attached & saturated', c.ratio_score === 1);
    assert('persistence_score attached', approx(c.persistence_score, 1));
    assert('max_ratio aggregated', approx(c.max_ratio, 2.25));
    assert('total_score in (0, 0.9]', c.total_score > 0 && c.total_score <= 0.9);
    // spectral discriminator coexists with the score.
    assert('likely_glint still present (thermal)', c.likely_glint === false);
}

console.log('\n[8] scoreThreshold drops low-quality clusters');
{
    const dets = [];
    for (const m of ['05', '06', '07', '08']) {
        for (const d of ['05', '15', '25']) {
            // flat ratio (~1.0) glint, single-pixel → low score
            dets.push({ date: `2024-${m}-${d}`, max_b12: 1.0, peak_b11: 1.0, b12_b11_ratio: 1.0,
                sun_elevation: 20, sun_azimuth: 150, pixels: 1, lon: -102, lat: 32 });
        }
    }
    const kept = clusterDetections(dets, { minDates: 4, minAvgB12: 0.5, scoreThreshold: 0.5 });
    assert('low-score cluster dropped at threshold 0.5', kept.length === 0);
    const all = clusterDetections(dets, { minDates: 4, minAvgB12: 0.5 });
    assert('same cluster kept with no threshold', all.length === 1);
}

console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail > 0 ? 1 : 0);
