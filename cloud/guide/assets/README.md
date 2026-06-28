# assets — imagery & geometry references for the 3D model

Reference material for modelling **Netia DC MIND, Jawczyce** (52.210771 °N, 20.848805 °E).
Provenance and licensing noted — **clear rights before any of this ships on the public
page.** These are research references, not cleared production assets.

| File | What | Source / rights |
|---|---|---|
| `netia-mind-exterior.jpg` | **Best reference** — full façade: 3-storey office wing + windowless data hall, ~4 loading bays, rooftop diesel-exhaust stacks + riser pipes, lightning masts, AHU plant | Netia/GSMonline opening coverage (21 Jun 2021), via fs.siteor.com — **press photo, © Netia/Polsat Plus; permission needed to publish** |
| `netia-mind-entrance.jpg` | Glazed reception + "NETIA DATA CENTER MIND" signage (ribbon-cutting) | same — © Netia/Polsat Plus |
| `netia-mind-opening-03.jpg` | Additional opening photo | same — © Netia/Polsat Plus |
| `netia-mind-dcd-thumb.jpg` | Small exterior thumbnail | Data Center Dynamics — © DCD |
| `netia-mind-sat-z18.png` | Satellite, zoom 18 — footprint + rooftop plant detail | Esri World Imagery tiles — © Esri/Maxar; attribution required |
| `netia-mind-sat-z17.png` | Satellite, zoom 17 — site context | Esri World Imagery — © Esri/Maxar |
| `osm-candidate-footprints.geojson` | OSM non-residential building footprints near the pin; likely data hall = `osm:949140207` (~1,630 m²) | OpenStreetMap, ODbL — © OSM contributors, attribution + share-alike |

## Geometry notes for modelling

- **Building** ≈ two flat-roofed boxes (L-shape): office wing 3 storeys (~12 m), data
  hall taller (~12–15 m), long axis NW–SE, footprint ~50–60 × 25–35 m. See
  [../sources/05-physical-specs.md](../sources/05-physical-specs.md) for the full
  buildable-geometry summary.
- **Footprint polygon:** start from `osm:949140207` in the geojson (sits on the pin, area
  matches the visual estimate); refine against the z18 satellite capture.
- **Signature detail:** the rooftop **mushroom-cap DRUPS exhaust stacks** + façade risers
  are the most recognisable feature — model them faithfully.

## To gather next
- Higher-res / orthorectified imagery — **Geoportal.gov.pl** (Polish national orthophoto,
  open data) for a pixel-accurate footprint and possibly building heights (LIDAR/NMT/NMPT).
- Street-level view (if any) and any Netia render/floorplan (likely needs a direct ask).
- Live Esri/Google tiles for a current capture (verify the cached imagery isn't
  mid-construction).
