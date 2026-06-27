"""Diagnostic probe over Ras Laffan + nearby region. Lists every pixel with
S5 > FLOOR with full multi-band readout — gives us a tunable view of where
the threshold should sit."""

from __future__ import annotations

import sys
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))
from reader import read_scene, crop_to_bbox  # noqa: E402

SEN3 = Path("data/S3B_SL_1_RBT____20250115T183636_20250115T183936_20250116T105351_0180_102_113_0360_PS2_O_NT_004.SEN3")

# Wider Qatar peninsula bbox — Ras Laffan is north tip, Doha/industrial in south
WIDE_BBOX = (50.7, 24.4, 51.8, 26.4)
RAS_LAFFAN_BBOX = (51.40, 25.80, 51.65, 26.00)

S5_FLOOR = 1.0  # very low to see whole distribution


def run(bbox, label):
    scene = read_scene(SEN3)
    cropped = crop_to_bbox(scene, bbox)
    valid = (
        np.isfinite(cropped.s5_rad) & np.isfinite(cropped.s6_rad)
        & np.isfinite(cropped.f1_bt) & np.isfinite(cropped.lat) & np.isfinite(cropped.lon)
    )
    print(f"\n=== {label}  shape={cropped.shape}  valid={valid.sum()} ===")

    # Background stats over valid pixels
    s5_vals = cropped.s5_rad[valid]
    f1_vals = cropped.f1_bt[valid]
    print(f"S5: median={np.median(s5_vals):.3f}  p99={np.percentile(s5_vals, 99):.3f}  max={s5_vals.max():.3f}")
    print(f"F1: median={np.median(f1_vals):.2f}K  p99={np.percentile(f1_vals, 99):.2f}K  max={f1_vals.max():.2f}K")

    hot = valid & (cropped.s5_rad > S5_FLOOR)
    n = hot.sum()
    print(f"pixels with S5 > {S5_FLOOR}: {n}")
    if n == 0:
        return

    rows, cols = np.where(hot)
    s5 = cropped.s5_rad[hot]
    order = np.argsort(-s5)
    print(f"  {'lat':>9} {'lon':>9}   {'S5':>6} {'S6':>6} {'F1_K':>7} {'S7_K':>7}  cloud_an")
    for k in order[:30]:
        r, c = rows[k], cols[k]
        print(f"  {cropped.lat[r,c]:9.4f} {cropped.lon[r,c]:9.4f}   "
              f"{cropped.s5_rad[r,c]:6.2f} {cropped.s6_rad[r,c]:6.2f} "
              f"{cropped.f1_bt[r,c]:7.2f} {cropped.s7_bt[r,c]:7.2f}  "
              f"{int(cropped.cloud_an[r,c])}")


run(RAS_LAFFAN_BBOX, "Ras Laffan tight bbox")
run(WIDE_BBOX, "Qatar peninsula wide bbox")
