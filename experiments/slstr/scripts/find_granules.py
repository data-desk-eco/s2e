"""Find SLSTR L1B granules over a point, filtered to plausible night-time.

Usage:
    uv run python scripts/find_granules.py --lon 51.55 --lat 25.91 \
        --start 2025-01-01 --end 2025-02-01

Prints one granule per line with size + sensing time + an estimated local hour.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))
from cdse import search  # noqa: E402


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--lon", type=float, required=True)
    ap.add_argument("--lat", type=float, required=True)
    ap.add_argument("--start", default="2025-01-01")
    ap.add_argument("--end", default="2025-02-01")
    ap.add_argument(
        "--platform", choices=["S3A", "S3B", None], default=None,
        help="Restrict to one platform; default = both",
    )
    ap.add_argument("--limit", type=int, default=50)
    args = ap.parse_args()

    grans = search(
        point=(args.lon, args.lat),
        start=args.start,
        end=args.end,
        platform=args.platform,
        limit=args.limit,
    )

    # Estimated local solar hour from longitude (rough).
    lon_offset_h = args.lon / 15.0

    print(f"{'name':<90} {'utc':<19} {'~local':<8} {'MB':>6}")
    for g in grans:
        local_h = (g.sensing_start.hour + g.sensing_start.minute / 60 + lon_offset_h) % 24
        is_night = local_h < 5 or local_h > 19
        marker = " *" if is_night else "  "
        print(
            f"{g.name:<90} "
            f"{g.sensing_start.strftime('%Y-%m-%dT%H:%M:%S')}  "
            f"{local_h:>5.1f}{marker} "
            f"{g.size_bytes/1e6:>6.0f}"
        )
    print(f"\n{len(grans)} granule(s). * = plausible night (local <05 or >19).")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
