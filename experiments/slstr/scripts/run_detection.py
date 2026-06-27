"""Run SLSTR flare detection on one or more .SEN3 directories.

Examples:
    # Detect over a specific bbox (Ras Laffan):
    uv run python scripts/run_detection.py \
        --bbox 51.40,25.80,51.65,26.00 \
        --out data/detections.csv \
        data/S3B_SL_1_RBT____*.SEN3

    # Auto-fetch a granule by ID first (requires CDSE_USERNAME/CDSE_PASSWORD):
    uv run python scripts/run_detection.py --fetch <granule-id> \
        --bbox 51.40,25.80,51.65,26.00
"""

from __future__ import annotations

import argparse
import csv
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))
from reader import read_scene, crop_to_bbox  # noqa: E402
from detect import detect_scene, detections_to_records  # noqa: E402


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("scenes", nargs="*", type=Path, help=".SEN3 directories")
    ap.add_argument("--bbox", help="W,S,E,N — crop scene to this region first")
    ap.add_argument("--out", type=Path, default=Path("data/slstr-detections.csv"))
    ap.add_argument(
        "--allow-day", action="store_true",
        help="Skip the night-time filter (for debugging only)",
    )
    args = ap.parse_args()

    if not args.scenes:
        ap.error("provide one or more .SEN3 directories")

    bbox = None
    if args.bbox:
        bbox = tuple(float(x) for x in args.bbox.split(","))
        if len(bbox) != 4:
            ap.error("--bbox needs 4 comma-separated floats")

    all_records: list[dict] = []
    for scene_dir in args.scenes:
        if not scene_dir.is_dir():
            print(f"skip (not a directory): {scene_dir}", file=sys.stderr)
            continue
        scene = read_scene(scene_dir)
        if bbox is not None:
            try:
                scene = crop_to_bbox(scene, bbox)
            except ValueError as e:
                print(f"skip {scene_dir.name}: {e}", file=sys.stderr)
                continue
        dets = detect_scene(scene, require_night=not args.allow_day)
        recs = detections_to_records(dets)
        print(f"{scene_dir.name}: {len(recs)} detection(s) "
              f"(scene shape {scene.shape})", file=sys.stderr)
        all_records.extend(recs)

    args.out.parent.mkdir(parents=True, exist_ok=True)
    if all_records:
        fieldnames = list(all_records[0].keys())
        with open(args.out, "w", newline="") as f:
            w = csv.DictWriter(f, fieldnames=fieldnames)
            w.writeheader()
            w.writerows(all_records)
        print(f"wrote {len(all_records)} detection(s) to {args.out}", file=sys.stderr)
    else:
        print("no detections", file=sys.stderr)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
