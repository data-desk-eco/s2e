"""SLSTR night-time flare detection — Caseiro-style high-accuracy class.

Pure function: arrays in, list of detections out. Mirrors the separation in
the JS lib's detect.js (typed arrays in, detections out, no I/O).

Detection logic:
  1. Filter to night by approximate local solar time (UTC + lon/15h).
  2. Mask cloud and missing data.
  3. Identify S5 (1.61um SWIR) candidates: radiance exceeds a granule-wide
     dynamic threshold (median + N*MAD) above an absolute floor.
  4. Require confirmation in >=1 of the auxiliary channels:
        S6 (2.25um SWIR)  -- another SWIR shoulder, similar background
        F1 (3.74um MIR)   -- thermal anomaly, BT well above background
  5. 4-connected component labelling on the confirmed mask.
  6. One detection per cluster: peak-S5 pixel's lat/lon, plus quality fields.

Thresholds are tunable constants below; defaults are conservative starting
points and should be refined against ground truth (Ras Laffan benchmark).
"""

from __future__ import annotations

from dataclasses import dataclass, asdict
from datetime import datetime, timezone

import numpy as np

# --- Tunable thresholds ---

# S5 (1.61um) primary detection. Radiance is mW m-2 sr-1 nm-1.
# Cold ocean/desert background at night is ~0 (calibration noise can be
# slightly negative). Empirically, p99 over a calm Qatar-sized scene
# is ~0.04, so a floor of 0.5 is ~13x above noise floor — safely catches
# faint sub-pixel flare contributions. MAD multiplier of 8 dominates
# whenever the scene has any background variability.
S5_ABS_FLOOR = 0.5
S5_MAD_MULT = 8.0

# S6 (2.25um) confirmation. Same radiance units; floor lower because S6
# is on the cold side of the flare Planck peak so the per-pixel signal
# is smaller for a given flare than S5.
S6_ABS_FLOOR = 0.3
S6_MAD_MULT = 8.0

# F1 (3.74um) thermal confirmation. Brightness temperature in K. We compare
# to local background (median across cloud-free pixels in a window).
F1_BT_DELTA_K = 4.0   # confirmed if pixel BT > scene background median + this
F1_BT_ABS_MIN = 285.0  # absolute floor; below this can't plausibly be a flare

# Solar geometry filter (rough). Local solar hour from UTC + lon/15.
# Caseiro algorithm requires night; descending S3 pass is ~22:00 LST.
NIGHT_HOUR_MIN = 19.0  # >= this hour OR <= NIGHT_HOUR_MAX
NIGHT_HOUR_MAX = 5.0


@dataclass
class Detection:
    lon: float
    lat: float
    date: str            # YYYY-MM-DD (UTC)
    sensing_utc: str     # full ISO timestamp
    max_s5: float
    mean_s5: float
    pixels: int
    bands_confirmed: int  # 1 or 2 (of S6, F1)
    f1_bt: float          # peak-pixel F1 BT (K), or NaN
    scene: str


def _local_solar_hour(utc_iso: str, lon_deg: float) -> float:
    dt = datetime.fromisoformat(utc_iso.replace("Z", "+00:00"))
    return (dt.astimezone(timezone.utc).hour
            + dt.minute / 60.0 + lon_deg / 15.0) % 24.0


def _is_night_local(hour: float) -> bool:
    return hour >= NIGHT_HOUR_MIN or hour <= NIGHT_HOUR_MAX


def _mad(x: np.ndarray) -> float:
    """Median absolute deviation, robust scale estimator. Ignores NaN."""
    finite = x[np.isfinite(x)]
    if finite.size == 0:
        return 0.0
    med = np.median(finite)
    return float(np.median(np.abs(finite - med)))


def _label_components_4conn(mask: np.ndarray) -> tuple[np.ndarray, int]:
    """4-connected component labels. Pure numpy + BFS; small images only.

    Returns (labels int32 array, n_components). Label 0 = background.
    """
    h, w = mask.shape
    labels = np.zeros((h, w), dtype=np.int32)
    next_label = 1
    flat_mask = mask.ravel()
    flat_labels = labels.ravel()
    for start in range(h * w):
        if not flat_mask[start] or flat_labels[start]:
            continue
        flat_labels[start] = next_label
        queue = [start]
        head = 0
        while head < len(queue):
            idx = queue[head]
            head += 1
            r, c = divmod(idx, w)
            if r > 0:
                n = idx - w
                if flat_mask[n] and not flat_labels[n]:
                    flat_labels[n] = next_label
                    queue.append(n)
            if r < h - 1:
                n = idx + w
                if flat_mask[n] and not flat_labels[n]:
                    flat_labels[n] = next_label
                    queue.append(n)
            if c > 0:
                n = idx - 1
                if flat_mask[n] and not flat_labels[n]:
                    flat_labels[n] = next_label
                    queue.append(n)
            if c < w - 1:
                n = idx + 1
                if flat_mask[n] and not flat_labels[n]:
                    flat_labels[n] = next_label
                    queue.append(n)
        next_label += 1
    return labels, next_label - 1


def detect_scene(
    scene, *, require_night: bool = True, mask_clouds: bool = False,
) -> list[Detection]:
    """Run detection on an in-memory Scene (see reader.Scene).

    require_night=False bypasses the local-solar-time filter — useful for
    forcing a run on an arguably-night granule near the dawn/dusk boundary.

    mask_clouds defaults False: SLSTR's bit-packed cloud_an variable lights
    up many of its histogram-based tests on bright flare pixels, masking
    real detections. Multi-band (S5+S6 / S5+F1) confirmation is the actual
    cloud-vs-fire discriminator. Pass True to force the mask anyway.
    """
    s5 = scene.s5_rad
    s6 = scene.s6_rad
    f1 = scene.f1_bt
    lat = scene.lat
    lon = scene.lon
    cloud = scene.cloud_an if mask_clouds else np.zeros_like(scene.cloud_an)

    # --- Night filter (per-scene crude check on the scene centroid) ---
    if require_night:
        valid = np.isfinite(lon)
        if valid.any():
            scene_lon = float(np.median(lon[valid]))
            hour = _local_solar_hour(scene.sensing_start_utc, scene_lon)
            if not _is_night_local(hour):
                return []
        else:
            return []

    # --- Valid-data mask ---
    valid = (
        np.isfinite(s5) & np.isfinite(s6) & np.isfinite(f1)
        & np.isfinite(lat) & np.isfinite(lon) & ~cloud
    )
    if not valid.any():
        return []

    # --- Granule-wide dynamic thresholds ---
    s5_med = float(np.median(s5[valid]))
    s5_mad = _mad(s5[valid])
    s5_thr = max(S5_ABS_FLOOR, s5_med + S5_MAD_MULT * s5_mad)

    s6_med = float(np.median(s6[valid]))
    s6_mad = _mad(s6[valid])
    s6_thr = max(S6_ABS_FLOOR, s6_med + S6_MAD_MULT * s6_mad)

    f1_med = float(np.median(f1[valid]))
    f1_thr = max(F1_BT_ABS_MIN, f1_med + F1_BT_DELTA_K)

    # --- Per-pixel detection mask ---
    s5_hit = valid & (s5 > s5_thr)
    if not s5_hit.any():
        return []
    s6_hit = s5_hit & (s6 > s6_thr)
    f1_hit = s5_hit & (f1 > f1_thr)
    confirmed = s5_hit & (s6_hit | f1_hit)
    if not confirmed.any():
        return []

    # --- Cluster + per-cluster summary ---
    labels, n = _label_components_4conn(confirmed)
    sensing_iso = scene.sensing_start_utc
    sensing_date = sensing_iso[:10]

    out: list[Detection] = []
    for k in range(1, n + 1):
        comp = labels == k
        npx = int(comp.sum())
        s5_vals = s5[comp]
        peak_idx = np.argmax(s5_vals)
        # Find absolute (row, col) of peak
        rows, cols = np.where(comp)
        pr, pc = int(rows[peak_idx]), int(cols[peak_idx])
        n_bands = int(s6_hit[pr, pc]) + int(f1_hit[pr, pc])
        out.append(
            Detection(
                lon=float(lon[pr, pc]),
                lat=float(lat[pr, pc]),
                date=sensing_date,
                sensing_utc=sensing_iso,
                max_s5=float(s5_vals.max()),
                mean_s5=float(s5_vals.mean()),
                pixels=npx,
                bands_confirmed=n_bands,
                f1_bt=float(f1[pr, pc]) if np.isfinite(f1[pr, pc]) else float("nan"),
                scene=scene.name,
            )
        )
    return out


def detections_to_records(dets: list[Detection]) -> list[dict]:
    return [asdict(d) for d in dets]
