# cloud/guide — the WAW3-2 data centre, researched

Research compendium for a planned **Data Desk** website page: a detailed analysis of the
technical infrastructure and supply chain behind our Sentinel-2 flare-detection pipeline,
centred on the data centre we actually run in. The eventual page wants a **3D model of the
data centre** for cool factor, backed by sourced fact.

This directory collects the **research and source documents** — every factual claim
carries its source URL, with marketing separated from verified engineering. It is a
reporting dossier, not yet the page itself.

## The one-paragraph version

Our `s2e` bulk pipeline runs on a CloudFerro OpenStack VM in the **WAW3-2** region,
co-located with the Copernicus `eodata` Sentinel archive. WAW3-2 is not a building — it's
CloudFerro's third-generation Warsaw cloud region. The **physical building is Netia's
"Data Center MIND" in Jawczyce**, ~10 km west of Warsaw (Sadowa 5, ~52.2108 °N
20.8488 °E), where CloudFerro is the anchor tenant. CloudFerro S.A. (Warsaw, founded 2015,
30 % owned by Innova Capital since 2024) leases colocation there and runs its own
OpenStack/Ceph stack; the deeper supply chain is Nvidia GPUs, a T-Systems-led ESA
consortium (the €150 m Copernicus Data Space Ecosystem contract), and the **coal-heavy
Polish grid** dressed in green-energy certificates.

## Headline facts (the spine of the page)

| | |
|---|---|
| **Region we rent** | CloudFerro `WAW3-2` (OpenStack Yoga, AMD) — config in `cloud/box.sh` |
| **Physical building** | **Netia Data Center MIND**, Sadowa 5, Jawczyce, Ożarów Mazowiecki |
| **Coordinates** | ≈ 52.210771 °N, 20.848805 °E |
| **Operator** | Netia S.A. (Polsat Plus group) — *not* Atman (common confusion) |
| **Cloud tenant** | CloudFerro S.A. (ASN AS200999), anchor tenant since 2021 |
| **Commissioned** | 1 April 2021; ~1,000 m² white space, ~520 racks, Tier III |
| **Backup power** | 2 × Hitec PowerPRO2700 DRUPS (diesel rotary UPS), ~2 MW each |
| **Grid reality** | Polish grid ≈ 600–670 gCO₂/kWh — most carbon-intensive in the EU |
| **The green gap** | CloudFerro 2024: market-based Scope 2 = 75 t vs location-based = **8,484 t** (113×) |
| **Real footprint** | Scope 3 ≈ 99 % of emissions; embodied carbon of hardware = 93 % |
| **eodata link** | WAW3-2 wired to the EODATA repository via multiple 100 Gbit/s links |

## The editorial angle

Two stories worth telling on the page:
1. **Sovereignty vs grid** — CloudFerro's whole pitch is *European sovereign cloud*, and
   it genuinely is (EU jurisdiction, GAIA-X, Copernicus). But "sovereign" and "clean" are
   different axes: the physical electrons are Polish coal, and "100 % renewable" is a
   certificate overlay over a ~671 gCO₂/kWh grid — visible in CloudFerro's *own* GHG
   report (a rare, quantified, self-disclosed greenwashing gap).
2. **The carbon is in the silicon** — for an EO/AI compute provider the operational
   electricity is dwarfed by the embodied carbon of the servers (93 % of CloudFerro's
   footprint). The honest infrastructure story is about hardware manufacture and refresh,
   not just PUE.

## Contents

| File | What it covers |
|---|---|
| [sources/01-identity-location.md](sources/01-identity-location.md) | What WAW3-2 is; the Netia MIND building; address, coords, eodata/CDSE/ESA links |
| [sources/02-netia-vs-atman-verdict.md](sources/02-netia-vs-atman-verdict.md) | The Netia-vs-Atman fact-check (name-collision red herring, resolved HIGH-confidence) |
| [sources/03-energy-emissions.md](sources/03-energy-emissions.md) | PUE, renewable claims, the GHG report, Polish-grid carbon intensity, EU reporting law |
| [sources/04-corporate-supply-chain.md](sources/04-corporate-supply-chain.md) | CloudFerro ownership, contracts, financials, suppliers (Nvidia, the CDSE consortium) |
| [sources/05-physical-specs.md](sources/05-physical-specs.md) | Netia MIND building/engineering specs + buildable geometry for the 3D model |
| [sources/cloudferro-2024-ghg-report.pdf](sources/cloudferro-2024-ghg-report.pdf) | **Primary document** — CloudFerro's audited 2024 GHG report (source of the §5 numbers) |
| [assets/](assets/) | Reference photos, satellite captures, OSM footprint geojson for the 3D model (see `assets/README.md` for rights) |

## For the 3D model

Target subject: the **single Netia MIND building at Jawczyce** — two conjoined volumes: a
**3-storey anthracite office wing** and a **tall windowless data-hall block** clad in
graphite composite cassette panels, its roof carrying the signature **mushroom-cap diesel
exhaust stacks** (the 2 × 2 MW Hitec DRUPS), dry-cooler arrays and lightning masts.
Verified against the opening photo in `assets/netia-mind-exterior.jpg`. Footprint ≈
50–60 × 25–35 m, long axis NW–SE — likely OSM polygon `949140207` (~1,630 m², on the pin)
in `assets/osm-candidate-footprints.geojson`. Full buildable-geometry summary in
`sources/05-physical-specs.md`. Coordinates ≈ 52.210771 °N, 20.848805 °E.
**Next:** Geoportal.gov.pl orthophoto + LIDAR for pixel-accurate footprint/heights; clear
press-photo rights before publishing (see `assets/README.md`).

## Open questions / next sources to chase

- **EU data-centre database (EED Art. 12)** entry for Netia MIND — legally-required PUE,
  WUE, waste-heat, renewable share. The authoritative, non-marketing figures.
- The exact Netia MIND **building dimensions / floor plan** (for an accurate model).
- Whether Netia's Innogy/E.ON wind PPA is **additional capacity** or just unbundled GOs.
- CloudFerro / Netia **net-zero target dates** (none currently published).
- Server **OEM** and the EODATA storage cluster's exact location within the campus.

---
*Compiled 2026-06-28 via multi-agent web research; every claim sourced in the linked
files. Treat operator marketing pages as claims, the GHG PDF and EU database as evidence.*
