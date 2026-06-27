"""Download a CDSE granule by ID into experiments/slstr/data/.

Requires CDSE_USERNAME and CDSE_PASSWORD env vars (free signup at
https://dataspace.copernicus.eu/).

Usage:
    uv run python scripts/fetch_granule.py <granule-id>
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))
from cdse import Granule, download, get_by_id, search  # noqa: E402


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("id_or_name", help="CDSE granule UUID or substring of name")
    ap.add_argument("--lon", type=float, default=51.55)
    ap.add_argument("--lat", type=float, default=25.91)
    ap.add_argument(
        "--data-dir", type=Path,
        default=Path(__file__).resolve().parents[1] / "data",
    )
    args = ap.parse_args()

    # If it looks like a UUID, fetch directly. Otherwise search and disambiguate.
    looks_like_uuid = (
        len(args.id_or_name) == 36 and args.id_or_name.count("-") == 4
    )
    if looks_like_uuid:
        g = get_by_id(args.id_or_name)
    else:
        # substring match on name
        candidates = [g for g in search(point=(args.lon, args.lat),
                                        start="2017-01-01", end="2099-01-01",
                                        limit=200)
                      if args.id_or_name in g.name]
        if not candidates:
            print("no granule name matched", file=sys.stderr)
            return 1
        if len(candidates) > 1:
            print(f"multiple matches; pick one:", file=sys.stderr)
            for c in candidates[:10]:
                print(f"  {c.id}  {c.name}", file=sys.stderr)
            return 1
        g = candidates[0]

    print(f"downloading {g.name} ({g.size_bytes/1e6:.0f} MB)...", file=sys.stderr)
    sen3 = download(g, args.data_dir)
    print(sen3)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
