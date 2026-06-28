# SLSTR flare detection (experimental)

Night-time flaring detection from Sentinel-3 SLSTR L1B, a night-time complement to
the day-time Sentinel-2 SWIR detection in this workspace. Kept in this subdir while
the methodology is being validated; will fold into the `s2-flares` CLI once reliable.

## Status

Experimental. Detection-only output (lat, lon, date, quality fields) — no FRP / temperature / area / volume
retrieval at this stage. Goal: confirm a flare was lit on a given night, parallel to the S2 day-time path.

## Methodology

Adapted from Caseiro et al. (2018, 2020) — SWIR-anchored hot-source detection on night-time S5 (1.61 µm),
confirmed in ≥1 of {S6, S7, F1}. Full Planck-curve retrieval is intentionally omitted for now.

References:
- Caseiro et al. 2020, *Gas flaring activity and BC emissions from Sentinel-3A SLSTR* (ESSD)
  https://essd.copernicus.org/articles/12/2137/2020/
- Caseiro et al. 2018, *A Methodology for Gas Flaring Detection and Characterisation Using SLSTR*
  https://www.preprints.org/manuscript/201805.0020/v1

## Setup

```sh
cd experiments/slstr
uv sync
```

## Layout

```
src/        # reader, detection algorithm
scripts/    # fetch + run entry points
data/       # granules + outputs (gitignored)
```
