# Source 01 — WAW3-2 identity & physical location

> Research note compiled 2026-06-28. **Verdict (HIGH confidence): the WAW3-2 site is
> Netia "Data Center MIND", Jawczyce — NOT Atman.** This was actively contested during
> research; the adjudication is recorded in [[02-netia-vs-atman-verdict]].

## 1. What "WAW3-2" refers to (region vs building)

**WAW3-2 is a cloud *region* — an independent OpenStack deployment — not a building
name.** CloudFerro runs four public clouds: WAW3-1, WAW3-2, WAW4-1 (Warsaw area) and
FRA1-2 (Frankfurt) ([cloudferro.com/cloud/our-public-clouds](https://cloudferro.com/cloud/our-public-clouds/)).
WAW3-1 and WAW3-2 are **two separate regions that physically share one site** (the
"WAW3 Data Center"); they "share power, colocation, and some aspects of internet
infrastructure, but their services, pricing, and management remain separate"
([WAW3-2/FRA1-2 announcement](https://cloudferro.com/news/cloudferro-waw3-2-and-fra1-2-regions-announced-for-creodias/)).

The "WAW3" denotes CloudFerro's **third Warsaw cloud generation / OpenStack region
instances** (WAW3-1, WAW3-2) — *not* Atman's "WAW-3" building (a name collision; see
[[02-netia-vs-atman-verdict]]).

- WAW3-2 is built on **OpenStack Yoga**, AMD-based, recommended for Earth-observation
  workloads; "connected to the EODATA main repository via multiple 100 Gbit/s links"
  ([same announcement](https://cloudferro.com/news/cloudferro-waw3-2-and-fra1-2-regions-announced-for-creodias/)).
  This is the in-region link our `/vsis3/eodata` JP2 reads traverse.
- WAW3-1 + WAW3-2 reached GA for CREODIAS customers in **June 2023**
  ([WAW3-1 GA](https://cloudferro.com/news/cloudferro-waw3-1-public-cloud-ga/)).

Our own config confirms `OS_REGION_NAME=WAW3-2`, S3 at `s3.WAW3-2.cloudferro.com`, auth
at `identity.cloudferro.com` (Keycloak realm `CloudFerro-Cloud`). CloudFerro's ASN is
**AS200999** ([bgp.tools](https://bgp.tools/as/200999)).

## 2. Physical operator — **Netia** (confirmed by primary sources)

- **CloudFerro's own infrastructure page** states WAW3-1/WAW3-2 are hosted in data
  centres **operated by Netia** ([cloudferro.com/cloud/data-centers](https://cloudferro.com/cloud/data-centers/);
  [creodias.eu/cloud/cloudferro-cloud](https://creodias.eu/cloud/cloudferro-cloud/)).
- **Netia's own page names CloudFerro as the anchor tenant** of its new DC: "The first
  client who … uses the collocation and leased lines services, 'remote hands' services
  and rents additional office space is **CloudFerro**" ([Netia — fills up at a record pace](https://www.netia.pl/en/operators/aktualnosci/netia-s-new-data-center-fills-up-at-a-record-pace)).
- Operator: **Netia S.A.**, part of the Polsat Plus / Cyfrowy Polsat group — a separate,
  unrelated company to Atman.

## 3. Physical address & coordinates

**Netia Data Center MIND — Sadowa 5, Jawczyce, 05-850 (Ożarów Mazowiecki), Poland**,
~10 km west of central Warsaw ([datacentermap — Netia MIND](https://www.datacentermap.com/poland/warsaw/netia-mind/);
[PeeringDB fac 12758](https://www.peeringdb.com/fac/12758)). Commissioned **1 April
2021**; ~1,000 m² white space across ~4 rooms, ~520 racks, Tier III
([DCD](https://www.datacenterdynamics.com/en/news/polands-netia-opens-data-center-near-warsaw/)).

**Coordinates: ≈ 52.210771° N, 20.848805° E** ([datacentermap](https://www.datacentermap.com/poland/warsaw/netia-mind/)).
(Jawczyce and the nearby village of Duchnice — where the unrelated Atman WAW-3 sits —
are both in Ożarów Mazowiecki, ~3 km apart; do not conflate.)

## 4. CloudFerro ↔ CDSE ↔ ESA ↔ eodata relationship

- **Copernicus Data Space Ecosystem (CDSE)** launched January 2023 as ESA's main
  Copernicus data-dissemination endpoint, replacing the Open Access Hub
  ([CDSE main endpoint news](https://cloudferro.com/news/copernicus-data-space-ecosystem-becomes-the-main-copernicus-data-dissemination-endpoint/)).
- Built/operated by an **ESA-selected consortium: T-Systems (lead), CloudFerro,
  Sinergise, VITO, DLR, ACRI-ST, RHEA** ([CDSE case](https://cloudferro.com/cases/copernicus-data-space-ecosystem/)).
  See [[04-corporate-supply-chain]] for the contract layer.
- The **"eodata" archive** (Sentinel-1/2/3/5P/6, Landsat, ENVISAT, Copernicus Services),
  public S3 endpoint `https://eodata.dataspace.copernicus.eu/`, >100 PB, is hosted by
  CloudFerro and co-located with its compute. WAW3-2 is explicitly wired to "the EODATA
  main repository via multiple 100 Gbit/s links."

## 5. Confidence / open questions

- **High confidence:** WAW3-2 is a region (not a building); WAW3-1 + WAW3-2 share one
  site; operator is **Netia**; site is **Netia DC MIND, Sadowa 5, Jawczyce**
  (~52.2108, 20.8488); CDSE/ESA/eodata relationships.
- **Medium confidence — exact building of the EODATA *storage* cluster:** CloudFerro
  describes WAW3-2 as *connected to* EODATA over 100 Gbit/s links — consistent with the
  petabyte EO Ceph cluster co-located at the same Netia campus, but not explicitly
  vendor-stated room-by-room.
- **Resolved red herring:** "Atman WAW-3" (Duchnice, opened Sept 2025) is a name
  collision, refuted by the June-2023 GA timeline — full reasoning in
  [[02-netia-vs-atman-verdict]].

## Source list
- https://cloudferro.com/cloud/data-centers/
- https://creodias.eu/cloud/cloudferro-cloud/
- https://www.netia.pl/en/operators/aktualnosci/netia-s-new-data-center-fills-up-at-a-record-pace
- https://cloudferro.com/cloud/our-public-clouds/
- https://cloudferro.com/news/cloudferro-waw3-2-and-fra1-2-regions-announced-for-creodias/
- https://cloudferro.com/news/cloudferro-waw3-1-public-cloud-ga/
- https://cloudferro.com/cases/copernicus-data-space-ecosystem/
- https://cloudferro.com/news/copernicus-data-space-ecosystem-becomes-the-main-copernicus-data-dissemination-endpoint/
- https://www.datacentermap.com/poland/warsaw/netia-mind/
- https://www.peeringdb.com/fac/12758
- https://www.datacenterdynamics.com/en/news/polands-netia-opens-data-center-near-warsaw/
- https://bgp.tools/as/200999
