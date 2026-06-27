"""Read a Sentinel-3 SLSTR L1B RBT .SEN3 directory.

SLSTR has two grids of interest for flare detection:
  - 500 m nadir 'an' grid for VIS/SWIR (S1-S6)
  - 1 km nadir 'in' grid for MIR/TIR (S7-S9, F1, F2) — values are BT in K

We resample the 1 km grid to the 500 m grid by 2x2 nearest-neighbor so all
bands share one frame. This is fine for sub-pixel point sources like flares.

Brightness temperature for thermal bands is left as-is (no Planck inversion);
detection works directly in BT space.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path

import numpy as np
import xarray as xr


@dataclass
class Scene:
    name: str
    sensing_start_utc: str  # ISO

    # 500 m nadir grid, all (rows, cols) same shape
    lat: np.ndarray         # float32
    lon: np.ndarray         # float32
    s5_rad: np.ndarray      # mW m-2 sr-1 nm-1, NaN where invalid
    s6_rad: np.ndarray
    s7_bt: np.ndarray       # K, NaN where invalid (resampled from 1km)
    f1_bt: np.ndarray       # K, NaN where invalid (resampled from 1km)
    cloud_an: np.ndarray    # bool, True where confident cloud (500m)

    @property
    def shape(self) -> tuple[int, int]:
        return self.lat.shape


def _open(nc: Path) -> xr.Dataset:
    return xr.open_dataset(nc, decode_cf=True, mask_and_scale=True)


def _band_500m(scene_dir: Path, var: str) -> np.ndarray:
    """Read a (rows, cols) 500m-grid variable, return float32 with NaN fills."""
    fname = f"{var}.nc"
    ds = _open(scene_dir / fname)
    arr = ds[var].values.astype(np.float32)
    ds.close()
    return arr


def _band_1km_to_500m(
    scene_dir: Path, var: str, target_shape: tuple[int, int]
) -> np.ndarray:
    """Read 1km 'in' grid variable and nearest-resample to 500m 'an' shape.

    SLSTR's 1km grid is exactly half the 500m grid in each axis. We expand
    by repeating each pixel 2x in both axes, then crop to target_shape.
    """
    ds = _open(scene_dir / f"{var}.nc")
    arr = ds[var].values.astype(np.float32)
    ds.close()
    expanded = np.repeat(np.repeat(arr, 2, axis=0), 2, axis=1)
    h, w = target_shape
    return expanded[:h, :w]


def _cloud_mask_an(scene_dir: Path) -> np.ndarray | None:
    """Read confident-cloud mask from flags_an.nc if present.

    SLSTR L1B flags use bit-packed cloud_an variable (UInt16). Bit 0 is
    'visible cloud confidence' in some product versions; the 'cloud_an'
    var aggregates multiple tests. Return True where any cloud test fires.
    """
    p = scene_dir / "flags_an.nc"
    if not p.exists():
        return None
    ds = _open(p)
    if "cloud_an" not in ds.variables:
        ds.close()
        return None
    cloud = ds["cloud_an"].values
    ds.close()
    # Any non-zero bit -> some cloud test fired
    return cloud.astype(np.uint16) != 0


def read_scene(scene_dir: Path) -> Scene:
    scene_dir = Path(scene_dir)
    if not scene_dir.is_dir():
        raise FileNotFoundError(scene_dir)

    geo = _open(scene_dir / "geodetic_an.nc")
    lat = geo["latitude_an"].values.astype(np.float32)
    lon = geo["longitude_an"].values.astype(np.float32)
    geo.close()

    target_shape = lat.shape

    s5 = _band_500m(scene_dir, "S5_radiance_an")
    s6 = _band_500m(scene_dir, "S6_radiance_an")
    # F1 (3.74um saturation-resistant fire channel) is on its own 'fn' stripe;
    # F2 (10.85um) and S7 share the 'in' stripe. Both 1km, both resample to 500m.
    s7 = _band_1km_to_500m(scene_dir, "S7_BT_in", target_shape)
    f1 = _band_1km_to_500m(scene_dir, "F1_BT_fn", target_shape)
    cloud = _cloud_mask_an(scene_dir)
    if cloud is None:
        cloud = np.zeros(target_shape, dtype=bool)
    else:
        cloud = cloud[: target_shape[0], : target_shape[1]]

    # Sensing time from filename:  S3X_SL_1_RBT____YYYYMMDDThhmmss_...
    m = re.search(r"_(\d{8}T\d{6})_", scene_dir.name)
    if not m:
        raise ValueError(f"cannot parse sensing time from {scene_dir.name}")
    raw = m.group(1)
    iso = (
        f"{raw[:4]}-{raw[4:6]}-{raw[6:8]}T{raw[9:11]}:{raw[11:13]}:{raw[13:15]}Z"
    )

    return Scene(
        name=scene_dir.name,
        sensing_start_utc=iso,
        lat=lat,
        lon=lon,
        s5_rad=s5,
        s6_rad=s6,
        s7_bt=s7,
        f1_bt=f1,
        cloud_an=cloud,
    )


def crop_to_bbox(scene: Scene, bbox: tuple[float, float, float, float]) -> Scene:
    """Return a new Scene cropped to the smallest row/col window containing bbox.

    bbox = (W, S, E, N) in WGS84 degrees. Useful for focusing on a single site.
    """
    w, s, e, n = bbox
    inside = (
        (scene.lon >= w) & (scene.lon <= e)
        & (scene.lat >= s) & (scene.lat <= n)
    )
    if not inside.any():
        raise ValueError(f"bbox {bbox} does not intersect scene {scene.name}")
    rows = np.where(inside.any(axis=1))[0]
    cols = np.where(inside.any(axis=0))[0]
    r0, r1 = rows.min(), rows.max() + 1
    c0, c1 = cols.min(), cols.max() + 1
    sl = (slice(r0, r1), slice(c0, c1))
    return Scene(
        name=scene.name,
        sensing_start_utc=scene.sensing_start_utc,
        lat=scene.lat[sl],
        lon=scene.lon[sl],
        s5_rad=scene.s5_rad[sl],
        s6_rad=scene.s6_rad[sl],
        s7_bt=scene.s7_bt[sl],
        f1_bt=scene.f1_bt[sl],
        cloud_an=scene.cloud_an[sl],
    )
