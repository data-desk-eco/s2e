"""CDSE (Copernicus Data Space Ecosystem) search + download.

Search is anonymous. Download requires CDSE credentials via env:
    CDSE_USERNAME, CDSE_PASSWORD
Free registration at https://dataspace.copernicus.eu/.
"""

from __future__ import annotations

import os
import zipfile
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

import requests

ODATA_BASE = "https://catalogue.dataspace.copernicus.eu/odata/v1"
TOKEN_URL = "https://identity.dataspace.copernicus.eu/auth/realms/CDSE/protocol/openid-connect/token"
DOWNLOAD_BASE = "https://zipper.dataspace.copernicus.eu/odata/v1"


@dataclass
class Granule:
    id: str
    name: str
    sensing_start: datetime
    size_bytes: int
    s3_path: str

    @property
    def night_local(self) -> bool:
        # Crude: SLSTR descending = ~22:00 LST. Real check uses solar zenith.
        # This filter only excludes obvious daytime ascending passes.
        return True  # caller should check sensing_start UTC against site longitude


def search(
    bbox: tuple[float, float, float, float] | None = None,
    point: tuple[float, float] | None = None,
    start: str = "2025-01-01",
    end: str = "2025-02-01",
    product_type: str = "SL_1_RBT",
    platform: str | None = None,
    limit: int = 50,
) -> list[Granule]:
    """Search CDSE for SLSTR granules. Anonymous, no auth needed.

    Either bbox=(W,S,E,N) or point=(lon,lat) — point is more reliable for
    irregular orbit footprints. start/end are ISO date strings (UTC).
    """
    if (bbox is None) == (point is None):
        raise ValueError("provide exactly one of bbox or point")

    if point is not None:
        lon, lat = point
        spatial = f"OData.CSC.Intersects(area=geography'SRID=4326;POINT({lon} {lat})')"
    else:
        w, s, e, n = bbox
        poly = f"POLYGON(({w} {s},{e} {s},{e} {n},{w} {n},{w} {s}))"
        spatial = f"OData.CSC.Intersects(area=geography'SRID=4326;{poly}')"

    name_filter = f"contains(Name,'{product_type}')"
    if platform:
        name_filter = f"startswith(Name,'{platform}') and {name_filter}"

    flt = (
        f"Collection/Name eq 'SENTINEL-3' and {name_filter} and {spatial} and "
        f"ContentDate/Start gt {start}T00:00:00.000Z and "
        f"ContentDate/Start lt {end}T00:00:00.000Z"
    )

    r = requests.get(
        f"{ODATA_BASE}/Products",
        params={"$filter": flt, "$top": str(limit), "$orderby": "ContentDate/Start"},
        timeout=60,
    )
    r.raise_for_status()
    items = r.json().get("value", [])

    out = []
    for it in items:
        out.append(
            Granule(
                id=it["Id"],
                name=it["Name"],
                sensing_start=datetime.fromisoformat(
                    it["ContentDate"]["Start"].replace("Z", "+00:00")
                ),
                size_bytes=it["ContentLength"],
                s3_path=it["S3Path"],
            )
        )
    return out


def get_by_id(granule_id: str) -> Granule:
    """Fetch metadata for a single granule by its CDSE UUID."""
    r = requests.get(f"{ODATA_BASE}/Products({granule_id})", timeout=30)
    r.raise_for_status()
    it = r.json()
    return Granule(
        id=it["Id"],
        name=it["Name"],
        sensing_start=datetime.fromisoformat(
            it["ContentDate"]["Start"].replace("Z", "+00:00")
        ),
        size_bytes=it["ContentLength"],
        s3_path=it["S3Path"],
    )


def _token() -> str:
    user = os.environ.get("CDSE_USERNAME")
    pw = os.environ.get("CDSE_PASSWORD")
    if not user or not pw:
        raise RuntimeError(
            "CDSE_USERNAME and CDSE_PASSWORD must be set. "
            "Register free at https://dataspace.copernicus.eu/ and export the creds."
        )
    r = requests.post(
        TOKEN_URL,
        data={
            "grant_type": "password",
            "username": user,
            "password": pw,
            "client_id": "cdse-public",
        },
        timeout=30,
    )
    r.raise_for_status()
    return r.json()["access_token"]


def download(granule: Granule, out_dir: Path) -> Path:
    """Download a granule .zip to out_dir, extract, return path to .SEN3 dir."""
    out_dir = Path(out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    sen3_dir = out_dir / granule.name
    if sen3_dir.exists():
        return sen3_dir

    zip_path = out_dir / f"{granule.name}.zip"
    if not zip_path.exists():
        token = _token()
        url = f"{DOWNLOAD_BASE}/Products({granule.id})/$value"
        with requests.get(
            url, headers={"Authorization": f"Bearer {token}"}, stream=True, timeout=600
        ) as r:
            r.raise_for_status()
            tmp = zip_path.with_suffix(".zip.part")
            with open(tmp, "wb") as f:
                for chunk in r.iter_content(chunk_size=1 << 20):
                    f.write(chunk)
            tmp.rename(zip_path)

    with zipfile.ZipFile(zip_path) as z:
        z.extractall(out_dir)
    zip_path.unlink()
    return sen3_dir
