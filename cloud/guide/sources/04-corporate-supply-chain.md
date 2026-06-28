# Source 04 — CloudFerro: corporate, ownership & supply chain

> Research note compiled 2026-06-28. The "who do we actually pay, and who do they
> pay" layer of the WAW3-2 supply chain. Building-independent — valid regardless of
> the Netia-vs-Atman colocation question (see [[01-identity-location]]).

## 1. Company, ownership & leadership

- **CLOUDFERRO S.A.** — Polish joint-stock company, KRS 0001049205 (converted from
  the earlier `sp. z o.o.`, KRS 0000543630) ([bizraport KRS](https://www.bizraport.pl/krs/0001049205/cloudferro-spolka-akcyjna)).
- **Founded 2015, Warsaw** ([Innova portfolio](https://innovacap.com/our-investments/portfolio/cloudferro)).
- **HQ:** ul. Nowogrodzka 31, 00-511 Warszawa; second office at Riverside Park, ul.
  Fabryczna 5 ([Innova-backed expansion](https://cloudferro.com/news/cloudferro-backed-by-innova-capital-in-further-european-expansion/)).
- **Founders:** Maciej Krzyżanowski, Stanisław Dałek, Michał Kasprzak ([InnovateCEE](http://www.innovatecee.com/business/a-small-company-cloud-ferro-gives-the-world/)).
  CEO **Maciej Krzyżanowski** is a co-founder and a **former CEO of ATM S.A.** (the
  Polish telecom/data-centre company historically tied to the Atman brand) — a PhD
  particle physicist ([CloudFerro](https://cloudferro.com/why-cloudferro/);
  [CB Insights](https://www.cbinsights.com/company/cloudferro/people)). **This ATM tie
  is significant for the Netia-vs-Atman question** ([[01-identity-location]]).
- **CTO / board:** Stanisław Dałek (CTO, board member); supervisory board incl.
  Ireneusz Stolarski ([CB Insights](https://www.cbinsights.com/company/cloudferro/people)).
- **Employees:** "over 200" (2024) ([Innova news](https://cloudferro.com/news/cloudferro-backed-by-innova-capital-in-further-european-expansion/)).

**Ownership — privately held:**
- **Innova Capital** (Central-European PE, founded 1994) took a **30 % minority stake**,
  announced 15 Feb 2024, via its Innova/7 fund — a "Founder Succession" deal
  ([Innova](https://innovacap.com/our-investments/portfolio/cloudferro);
  [AIN](https://en.ain.ua/2024/02/15/innova-capital-acquires-polish-cloudferro/)).
- Remaining ~70 % with founders / undisclosed shareholders; exact splits not public.
  No publicly-traded parent ([Crunchbase](https://www.crunchbase.com/organization/cloudferro)).

## 2. Major contracts (the public-sector revenue base)

- **Copernicus Data Space Ecosystem (CDSE)** — flagship. **6-year contract
  (extendable to 10), €150 million**, consortium of **seven** orgs **led by T-Systems**
  (Deutsche Telekom): **T-Systems, CloudFerro, Sinergise, VITO, DLR, ACRI-ST, RHEA**
  ([CloudFerro/Telekom](https://www.telekom.com/en/media/media-information/archive/copernicus-data-space-1024098)).
  T-Systems + CloudFerro provide the cloud infrastructure; CDSE is now the **main
  Copernicus dissemination endpoint** ([CloudFerro](https://cloudferro.com/news/copernicus-data-space-ecosystem-becomes-the-main-copernicus-data-dissemination-endpoint/)).
- **CREODIAS / DIAS (2017–18)** — CloudFerro in a Creotech-led consortium that won one
  of five Copernicus DIAS platforms (~€15m each); CloudFerro operates CREODIAS, now the
  commercial arm of CDSE ([EARSC](https://earsc.org/2017/12/15/polish-cloudferro-wins-copernicus-dias/)).
- **EUMETSAT WEkEO** — CloudFerro + Thales Alenia Space, **>€10m** ([CREODIAS](https://creodias.eu/news/cloudferro-and-its-partners-are-building-copernicus-data-access-service/)).
- **EUMETSAT DestinE Data Lake** (EU Destination Earth) — CloudFerro **prime
  contractor**, **>60 PB storage + >23,500 CPUs** ([CloudFerro](https://cloudferro.com/news/cloudferro-contributes-to-eus-destination-earth-initiative/)).
- **ECMWF** — €1.3m hybrid-cloud contract; also relocated ECMWF's cloud to Bologna
  ([eomag](https://eomag.eu/cloudferro-with-a-13-million-contract-from-ecmwf/)).
- Customer roster: "trusted by" **ESA, ECMWF, EUMETSAT** ([CloudFerro](https://cloudferro.com/why-cloudferro/)).

## 3. The eodata / EO archive relationship

- CloudFerro **operates and hosts** the EO repository underlying CDSE/CREODIAS — one of
  the largest open & free EO repositories worldwide, **>100 petabytes**
  ([CDSE service page](https://dataspace.copernicus.eu/ecosystem/services/cloudferro-cloud)).
- CREODIAS repository cited at **>67 PB growing ~26 TB/day** ([CloudFerro case](https://cloudferro.com/cases/creodias/));
  CDSE projected toward 85–100 PB over six years.
- Holdings: full Sentinel family (S1/S2/S3/S5P/S6), historical Landsat 5/7/8, ENVISAT,
  SMOS, Copernicus Contributing Missions. **WAW3-2 is connected to the EODATA main
  repository via multiple 100 Gbit/s links** ([WAW3-2 announcement](https://cloudferro.com/news/cloudferro-waw3-2-and-fra1-2-regions-announced-for-creodias/))
  — the concrete fact underpinning this repo's `/vsis3/eodata` in-region co-location.

## 4. Cloud footprint & colocation model

- **Public cloud regions:** WAW3-1, WAW3-2, WAW4-1 (Warsaw area) and FRA1-2 (Frankfurt)
  ([CREODIAS](https://creodias.eu/cloud/cloudferro-cloud/)).
- WAW3-1 + WAW3-2 share one "WAW3 Data Center"; independent regions. WAW3-2 (OpenStack
  Yoga, AMD) is the EO-recommended region — the one we rent.
- **Model: CloudFerro colocates** — owns/operates its own hardware stack (OpenStack,
  Ceph, Kubernetes, self-assembled in-house) inside a third-party data centre rather
  than owning buildings ([CDSE service page](https://dataspace.copernicus.eu/ecosystem/services/cloudferro-cloud)).
  **Which** operator hosts WAW3 is contested (Netia vs Atman) — see [[01-identity-location]].

## 5. EU-sovereignty positioning (marketing, factually grounded)

- **GAIA-X** member ([CloudFerro](https://cloudferro.com/news/cloudferro-joins-the-european-gaia-x-project/)).
- **Virt8ra** sovereign multi-cloud participant ([DCD](https://www.datacenterdynamics.com/en/news/six-more-european-providers-join-virt8ra-sovereign-cloud-offering/)).
- Pitch: "a sovereign European cloud … fully European company … runs exclusively in
  EU-based data centers under European jurisdiction" — its core differentiator vs the
  US hyperscalers ([CDSE service page](https://dataspace.copernicus.eu/ecosystem/services/cloudferro-cloud)).
- The broader EU funding frame is **IPCEI-CIS** (Next-Gen Cloud, approved Dec 2023), but
  **no source confirms CloudFerro is a direct IPCEI-CIS aid recipient** — treat as
  unverified.

## 6. Financials

- **2024 revenue ~€41 million** — company record ([itwiz](https://itwiz.pl/spolka-cloudferro-odnotowala-ok-41-mln-euro-przychodow-za-2024-rok/)).
- **2023 (KRS):** revenue 159.7m PLN (~€36m), net profit 36.1m PLN, EBITDA 66.7m PLN,
  total assets 180.1m PLN; ROE ~49 %, net margin ~23 % ([bizraport](https://www.bizraport.pl/krs/0001049205/cloudferro-spolka-akcyjna)).
- **2022:** revenue 69.1m PLN — revenue more than doubled 2022→2023. (Crunchbase's older
  "$14.7m" figure is stale.)
- Revenue is overwhelmingly **public-sector / institutional** (ESA, EUMETSAT, ECMWF, EC
  Copernicus).

## 7. Key suppliers (the deeper supply chain)

- **Colocation / data-centre operator:** Netia *or* Atman — contested, see [[01-identity-location]].
- **GPU / compute hardware:** **Nvidia** — H100 94GB in WAW4-1, H200 141GB planned, also
  L40S and A6000, up to 8 GPUs/server passthrough ([CloudFerro GPU](https://cloudferro.com/cloud/public-cloud/compute/gpu/)).
- **Software stack:** open source — OpenStack (Yoga), Ceph, Kubernetes; self-assembled to
  avoid lock-in ([CDSE service page](https://dataspace.copernicus.eu/ecosystem/services/cloudferro-cloud)).
- **Consortium partners doubling as supply chain:** T-Systems/Deutsche Telekom, Sinergise
  (now part of Planet Labs), VITO, DLR, ACRI-ST, RHEA, Thales Alenia Space, Creotech.
- **Unidentified:** server OEM (Dell/Supermicro/etc.), network/transit providers, the
  FRA1 colocation operator.

## Confidence / open questions

- **High confidence:** founding/HQ/founders, Innova 30 % stake, the €150m CDSE consortium,
  ECMWF/WEkEO/DestinE contracts, 100+ PB archive, Nvidia GPUs, 2023/24 revenue.
- **Opaque:** exact shareholder split beyond Innova's 30 %; whether the EU is an
  *equity* investor (one secondary profile claims so — likely conflates contracts with
  equity; unverified); IPCEI-CIS direct funding; 2024 net profit; FRA1 operator; server OEM.
- **Worth a deeper trace for the article:** the Krzyżanowski ↔ ATM S.A. history and
  Atman's own ownership (Goldman Sachs / Global Compute Infrastructure interests over
  time) — relevant to whether "WAW3" colocation is Atman after all.

## Source list
- https://www.bizraport.pl/krs/0001049205/cloudferro-spolka-akcyjna
- https://innovacap.com/our-investments/portfolio/cloudferro
- https://cloudferro.com/news/cloudferro-backed-by-innova-capital-in-further-european-expansion/
- https://en.ain.ua/2024/02/15/innova-capital-acquires-polish-cloudferro/
- https://cloudferro.com/why-cloudferro/
- https://www.cbinsights.com/company/cloudferro/people
- https://www.telekom.com/en/media/media-information/archive/copernicus-data-space-1024098
- https://cloudferro.com/news/copernicus-data-space-ecosystem-becomes-the-main-copernicus-data-dissemination-endpoint/
- https://earsc.org/2017/12/15/polish-cloudferro-wins-copernicus-dias/
- https://creodias.eu/news/cloudferro-and-its-partners-are-building-copernicus-data-access-service/
- https://cloudferro.com/news/cloudferro-contributes-to-eus-destination-earth-initiative/
- https://eomag.eu/cloudferro-with-a-13-million-contract-from-ecmwf/
- https://dataspace.copernicus.eu/ecosystem/services/cloudferro-cloud
- https://cloudferro.com/cases/creodias/
- https://creodias.eu/cloud/cloudferro-cloud/
- https://cloudferro.com/cloud/public-cloud/compute/gpu/
- https://itwiz.pl/spolka-cloudferro-odnotowala-ok-41-mln-euro-przychodow-za-2024-rok/
