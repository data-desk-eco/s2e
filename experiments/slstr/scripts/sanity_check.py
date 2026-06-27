"""Quick sanity check: how many detections fall over open water far from
any plausible flaring infrastructure? A working detector should put ~0
hits in the open Persian Gulf or open Arabian Sea.

We can't easily do an exact land-mask without more deps, so we use a crude
rule: if a detection's nearest neighbor is >50 km away, it's isolated and
plausibly a false positive. Real flaring forms clusters."""

from __future__ import annotations

import csv
import math
import sys
from collections import Counter

CSV = "data/full-granule-slstr.csv"


def haversine(a, b):
    R = 6371.0
    lat1, lon1 = math.radians(a[0]), math.radians(a[1])
    lat2, lon2 = math.radians(b[0]), math.radians(b[1])
    dlat = lat2 - lat1
    dlon = lon2 - lon1
    h = math.sin(dlat / 2)**2 + math.cos(lat1) * math.cos(lat2) * math.sin(dlon / 2)**2
    return 2 * R * math.asin(math.sqrt(h))


def main():
    with open(CSV) as f:
        rows = list(csv.DictReader(f))
    pts = [(float(r["lat"]), float(r["lon"]), float(r["max_s5"]),
            int(r["bands_confirmed"]), float(r["f1_bt"]), int(r["pixels"])) for r in rows]
    print(f"{len(pts)} detections in granule")

    # Distribution of S5 max
    s5s = [p[2] for p in pts]
    print(f"S5 max: min={min(s5s):.2f}  median={sorted(s5s)[len(s5s)//2]:.2f}  max={max(s5s):.2f}")

    # bands_confirmed distribution
    bc = Counter(p[3] for p in pts)
    print(f"bands_confirmed: {dict(bc)}")

    # Cluster count by pixels
    px = Counter(p[5] for p in pts)
    print(f"cluster pixel counts: {dict(sorted(px.items()))}")

    # Find isolated detections (nearest-neighbor > 30 km) — possible FPs
    isolated = []
    for i, p in enumerate(pts):
        min_d = float("inf")
        for j, q in enumerate(pts):
            if i == j:
                continue
            d = haversine((p[0], p[1]), (q[0], q[1]))
            if d < min_d:
                min_d = d
        if min_d > 30:
            isolated.append((min_d, p))
    isolated.sort(reverse=True)
    print(f"\nisolated detections (NN > 30 km): {len(isolated)}")
    for dist, p in isolated[:10]:
        print(f"  ({p[0]:.4f}, {p[1]:.4f})  S5={p[2]:.2f}  F1={p[4]:.1f}K  "
              f"px={p[5]}  bands={p[3]}  NN={dist:.0f}km")


if __name__ == "__main__":
    main()
