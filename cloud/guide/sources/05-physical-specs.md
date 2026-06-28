# Source 05 — Netia DC MIND: physical & engineering specs (for the 3D model)

> Research note compiled 2026-06-28. The buildable subject. Confirmed against the
> official opening photo (`assets/netia-mind-exterior.jpg`, viewed) and Esri satellite.
> Subject is **Netia DC MIND, Jawczyce** — see [[01-identity-location]]; *not* Atman.

**Facility:** Netia Data Center MIND · **Operator:** Netia S.A. (Grupa Polsat Plus) ·
**Address:** Sadowa 5, Jawczyce 05-850, Ożarów Mazowiecki · **PeeringDB:** fac/12758,
code OZAIC001 · **Coords:** 52.210771 °N, 20.848805 °E · **Commissioned:** early April
2021 (ceremonial opening 21 June 2021) · **Anchor tenant:** CloudFerro (WAW3 / eodata).
([PeeringDB](https://www.peeringdb.com/fac/12758); [datacentermap](https://www.datacentermap.com/poland/warsaw/netia-mind/);
[Netia](https://www.netia.pl/en/operators/aktualnosci/netia-s-new-data-center-fills-up-at-a-record-pace))

## 1. Architecture & exterior (confirmed from photo)

Two conjoined flat-roofed orthogonal volumes forming an L / stepped mass:
- **(A) Office/admin wing** — **3 storeys**, three rows of ribbon windows, glazed
  ground-floor reception with a flat entrance canopy; backlit "NETIA DATA CENTER MIND"
  sign + yellow origami-bird Netia logo.
- **(B) Data-hall block** — tall, **single-volume, windowless**, taller than the office
  wing; a row of **~4 large roller-shutter loading bays** on the paved apron side.
- **Cladding:** large **anthracite / graphite composite cassette panels** across both
  volumes; dark aluminium-framed glazing. Modern industrial-minimal.
- **Data-hall roofline (load-bearing detail for the model):** **3–4 tall cylindrical
  "mushroom-cap" diesel-exhaust stacks** (the DRUPS flues) with vertical **riser pipes
  running up the façade**; **lightning-protection masts** at roof corners; a band of
  light-coloured **louvered AHU/cooling units** in the valley between the two volumes;
  **dry-cooler / condenser arrays** on the data-hall roof (grid of low boxy units, from
  satellite).
- **Site/yard:** paved concrete-block apron wrapping south and east; lawn strip to the
  SW (where the map pin sits); scattered utility/manhole covers; open light-industrial
  plot with reserved expansion land. No dramatic fence in imagery — assume light
  security fence.
- **Architect:** not published. **Contractor:** "a company from the CP Group" (Cyfrowy
  Polsat group) ([whatnext.pl](https://whatnext.pl/netia-buduje-data-center-to-kolejny-taki-budynek-w-warszawie/)).

Photo: `assets/netia-mind-exterior.jpg` (full façade), `assets/netia-mind-entrance.jpg`
(signage), satellite `assets/netia-mind-sat-z18.png` / `-z17.png`.

## 2. Floor area

- **White space:** "over 1,000 m²" of server space, **4 chambers (data halls)**, "nearly
  520 racks" ([Netia](https://www.netia.pl/en/operators/aktualnosci/netia-s-new-data-center-fills-up-at-a-record-pace);
  [DCD](https://www.datacenterdynamics.com/en/news/polands-netia-opens-data-center-near-warsaw/)).
- **Office:** ~700 m² leasable ([itwiz](https://itwiz.pl/cloudferro-pierwszym-klientem-nowego-data-center-netii/)) vs
  "over 1,400 m²" total non-server area ([Netia build PR](https://my.netia.pl/pr/550005/netia-buduje-najnowoczesniejsze-data-center-w-aglomeracji-warszawskiej)).
- **Total building m² / dimensions: not published.** Reserved expansion land on the plot.

**Footprint cross-check:** the spec-sheet visual estimate (~50–60 m × ~25–35 m, long
axis NW–SE) matches the **1,630 m² OSM building footprint sitting almost exactly on the
map pin** (`osm:949140207` at 52.210871, 20.848803) — the most likely data-hall polygon
in `assets/osm-candidate-footprints.geojson`. An adjacent 8,844 m² "industrial" polygon
and 3,888–4,363 m² "commercial" polygons are the surrounding plot/neighbours, not the
DC. Treat the OSM ID as a strong candidate, not a verified survey.

## 3. Power & backup

- **Topology:** three-line architecture presented as full **2N** — one static UPS line +
  **two DRUPS lines** ([Netia build PR](https://my.netia.pl/pr/550005/netia-buduje-najnowoczesniejsze-data-center-w-aglomeracji-warszawskiej)).
- **DRUPS:** **2 × Hitec PowerPRO2700** diesel-rotary UPS, stated **2 MW each** ([Netia](https://www.netia.pl/en/operators/aktualnosci/netia-s-new-data-center-fills-up-at-a-record-pace);
  [itwiz](https://itwiz.pl/cloudferro-pierwszym-klientem-nowego-data-center-netii/)).
- **What a PowerPRO2700 is (for a cutaway):** a long **horizontal cylindrical machine** on
  a common shaft — 16-cylinder diesel + KEM flywheel (kinetic-energy store, replacing
  batteries) + alternator, clutch-coupled. Flywheel rides through the gap; diesel starts
  in seconds; exhausts via the rooftop stacks. 50 Hz rating ~1,760 kW / 2,200 kVA; "2700"
  = 2,700 kVA at 60 Hz ([Hitec PowerPRO](https://hitec-ups.com/products/powerpro-series/)).
- **Supply:** 100 % green/renewable energy via wind purchase agreements ([Polsat Plus PR](https://grupapolsatplus.pl/pl/archive/netia-rekordowo-szybko-wypelnia-nowe-w-pelni-zasilane-zielona-energia-data-center-pod))
  — but see [[03-energy-emissions]]: this is a certificate overlay on the coal-heavy
  Polish grid.
- **Total IT/utility capacity, transformer count, MV feed voltage: not published**
  (two 2-MW DRUPS lines imply low-single-digit MW).

## 4. Cooling

- **Externally visible (model-relevant):** rooftop **dry-cooler / condenser arrays** +
  **louvered AHU units** between the two volumes. No published chiller make/model,
  CRAC/CRAH counts, free-cooling %, or aisle-containment type ([Netia](https://www.netia.pl/en/operators/aktualnosci/netia-s-new-data-center-fills-up-at-a-record-pace)).

## 5. Redundancy / Tier / certifications

- **Tier III**, designed to **EN/PN 50600 Class 3** with many elements to **Class 4**;
  guaranteed availability **99.982 %** ([Netia](https://www.netia.pl/en/operators/aktualnosci/netia-s-new-data-center-fills-up-at-a-record-pace);
  [datacenters.com](https://www.datacenters.com/netia-jawczyce)).
- Full **2N** power. Management: **ISO 27001, ISO 9001** + a critical-infrastructure
  certificate ([datacentermap](https://www.datacentermap.com/poland/warsaw/netia-mind/)).
- No public Uptime Institute / ANSI-TIA-942 plate — design references EN 50600.

## 6. Connectivity

- **Carrier-neutral**, dual/redundant fibre entries; **9 connected networks** per
  PeeringDB (Netia AS12741, Fiberax, Internet Invest, Omega Telecom, T-Band, Volz,
  Giganet/Ukrainian Backbone); on-site **Giganet Internet Exchange** ([PeeringDB](https://www.peeringdb.com/fac/12758)).
- **"MIND" branding** = Netia's body/mind DC naming scheme: **MIND** (Jawczyce), **BRAIN**
  (Grodzisk Maz.), **HEART** (Warsaw, Poleczki 13), **SOUL** (Kraków) ([Netia BRAIN](https://www.netia.pl/en/operators/aktualnosci/nowe-data-center-brain)).

## 7. Cost, timeline, expansion

- **Cost:** ~**PLN 79 million** (some sources ~70 m) ([wirtualnemedia](https://www.wirtualnemedia.pl/artykul/netia-buduje-nowe-centrum-danych-koszt-79-mln-zl-start-w-2021-roku)).
- **Timeline:** announced Aug 2020 → operational early April 2021 → ceremonial opening 21
  June 2021; **32 % commercialised within 3 weeks, ~40 % within ~3 months** ([Netia](https://www.netia.pl/en/operators/aktualnosci/netia-s-new-data-center-fills-up-at-a-record-pace)).
- **Standalone** facility (not a multi-building campus at this address), deliberately sited
  away from railways and fuel stations ([whatnext.pl](https://whatnext.pl/netia-buduje-data-center-to-kolejny-taki-budynek-w-warszawie/)).

## For the 3D model — buildable geometry

- **Massing:** two flat-roofed boxes, L/stepped. (A) office wing 3 storeys (~12 m); (B)
  data hall taller (~12–15 m), blank/windowless. Both in anthracite composite cassette
  panels + dark glazing.
- **Data-hall ground floor:** ~4 roller-shutter loading bays on the apron side.
- **Rooftop (data hall):** 3–4 tall cylindrical mushroom-cap exhaust stacks + façade riser
  pipes; lightning masts at corners; dry-cooler/condenser arrays; louvered AHU band in the
  valley between volumes.
- **Site:** paved block apron south+east; SW lawn; utility covers; reserved expansion land;
  light perimeter fence.
- **DRUPS:** model as internal horizontal cylindrical skids (cutaway) exhausting to the
  roof stacks — not an external genset yard.
- **Footprint:** ~50–60 m × ~25–35 m, long axis NW–SE (≈ OSM `949140207`, 1,630 m²). All
  dimensions **estimates** — none published.

## Confidence / open questions

- **High confidence:** identity, 1,000 m² / 4 halls / ~520 racks, 2×Hitec PowerPRO2700
  DRUPS, three-line 2N, Tier III / EN 50600, 100 % green (certificate), PLN 79 m, 2021,
  charcoal façade + 3-storey office + data hall + rooftop stacks/dry-coolers, CP-Group
  contractor, carrier-neutral + Giganet IX.
- **Open / undocumented:** exact building dimensions & data-hall height (visual estimates
  only); architect; total IT power / MV feed / transformer count; cooling make/counts/
  free-cooling %; office area 700 vs 1,400 m²; cost 79 vs 70 m; no Uptime/TIA plate;
  whether Esri imagery is fully current. The **EU EED Art. 12 database** is the best lever
  for verified PUE/power numbers — see [[03-energy-emissions]] §8.

## Source list
- https://www.peeringdb.com/fac/12758
- https://www.datacentermap.com/poland/warsaw/netia-mind/
- https://www.netia.pl/en/operators/aktualnosci/netia-s-new-data-center-fills-up-at-a-record-pace
- https://www.datacenterdynamics.com/en/news/polands-netia-opens-data-center-near-warsaw/
- https://www.datacenters.com/netia-jawczyce
- https://itwiz.pl/cloudferro-pierwszym-klientem-nowego-data-center-netii/
- https://my.netia.pl/pr/550005/netia-buduje-najnowoczesniejsze-data-center-w-aglomeracji-warszawskiej
- https://hitec-ups.com/products/powerpro-series/
- https://grupapolsatplus.pl/pl/archive/netia-rekordowo-szybko-wypelnia-nowe-w-pelni-zasilane-zielona-energia-data-center-pod
- https://whatnext.pl/netia-buduje-data-center-to-kolejny-taki-budynek-w-warszawie/
- https://www.wirtualnemedia.pl/artykul/netia-buduje-nowe-centrum-danych-koszt-79-mln-zl-start-w-2021-roku
- https://www.netia.pl/en/operators/aktualnosci/nowe-data-center-brain
- Photos: fs.siteor.com/gsmonline/files/982_newsy_06_2021/{01,02,03}_netia_20210621.jpg
