# SLSTR experiment — findings (2026-04-27)

## What works

End-to-end pipeline produces plausible per-night flare detections:
1. CDSE OData search (anonymous) finds night granules over a point.
2. CDSE authenticated download (`CDSE_USERNAME`/`CDSE_PASSWORD`) fetches `.SEN3.zip`.
3. `reader.py` parses S5/S6 (500 m an grid) + S7/F1 (1 km in/fn grids → 2× nearest to 500 m) +
   `cloud_an` flags.
4. `detect.py` — granule-wide median+MAD threshold on S5, confirmation in ≥1 of {S6, F1},
   4-connected component clustering, one detection per cluster with peak lat/lon.

## Validation summary

| check | result |
|---|---|
| Ras Laffan, 3 consecutive nights (Jan 15/16/17 2025) | 1, 2, 1 detections per night, all within ~5 km of the LNG complex |
| Full Persian Gulf granule (Jan 15) | 177 detections; brightest hits all at known flaring sites (Ras Laffan, South Pars/North Dome, Saudi Ghawar, UAE offshore) |
| Daytime pass over Ras Laffan | 0 detections (night-filter correct) |
| Open-ocean pixels | no detections in non-hydrocarbon ocean regions |

## Calibrated defaults (in `src/detect.py`)

Empirically tuned against the Jan 15 Persian Gulf granule:

| constant | value | rationale |
|---|---|---|
| `S5_ABS_FLOOR` | 0.5 | scene p99 over 144k pixels was 0.04 — floor is ~13× noise |
| `S5_MAD_MULT` | 8.0 | dominates only when scene background has variance |
| `S6_ABS_FLOOR` | 0.3 | S6 is on cold side of flare Planck peak; lower floor than S5 |
| `S6_MAD_MULT` | 8.0 | as above |
| `F1_BT_DELTA_K` | 4.0 | typical sub-pixel flare gives +20–50 K F1 anomaly; +4 is conservative |
| `F1_BT_ABS_MIN` | 285.0 | reject "cold" pixels that can't be a flare |
| `NIGHT_HOUR_MIN`/`MAX` | 19.0 / 5.0 | crude UTC+lon/15 night filter |

`mask_clouds` defaults **off**: SLSTR's bit-packed `cloud_an` lights up its
SWIR-histogram tests on bright flare pixels, masking real detections. The
multi-band confirmation is the actual fire-vs-cloud discriminator.

## Output schema

CSV per detection (one row per pixel-cluster):

```
lon, lat, date, sensing_utc, max_s5, mean_s5, pixels, bands_confirmed, f1_bt, scene
```

`bands_confirmed` is 1 (S5+S6 only or S5+F1 only) or 2 (both). Downstream
consumers can post-filter to `bands_confirmed == 2` for stricter mode (~50%
of Persian Gulf detections meet this).

## Known limitations

- **Cross-night spatial drift.** Same physical flare lands in slightly different
  500 m pixels on different orbits (combination of pointing variation + which flares
  are actually lit on a given night). For "this *site* was lit on this date" the
  output is correct; for tight per-pixel tracking, cross-date clustering is needed
  (analogous to `clusterDetections` in the JS lib).
- **Cluster geometry.** Tightly-spaced flare farms (Ras Laffan) collapse to one
  multi-pixel cluster — we report one detection at the peak. Resolving individual
  flares within ~500 m is fundamentally limited by SLSTR's instrument.
- **No FRP/temperature/area retrieval.** Detection-only by design; the dual
  Planck fit from Caseiro 2018 is intentionally omitted.
- **Threshold tuning.** Defaults are calibrated against one Persian Gulf
  night. Polar/high-latitude scenes or areas with strong moonlit cloud may
  need higher floors.

## Open questions for next iteration

1. Should we add cross-date clustering (port `lib/cluster.js`)?
2. Should `bands_confirmed >= 2` be the default, with single-band as opt-in?
3. Is the 1 km → 500 m 2× nearest resampling accurate enough? A small offset
   would mean F1 confirmations are misaligned by up to 500 m — worth checking
   with `cartesian_an.nc` / `cartesian_in.nc` if precision matters.
4. Folding into the unified `s2-flares` CLI — what subcommand shape?
