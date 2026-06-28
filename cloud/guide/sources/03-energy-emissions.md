# Source 03 — Energy & emissions performance

> Research note compiled 2026-06-28. The most editorially important strand: this is
> where the "100 % renewable" marketing meets the coal-heavy Polish grid. Primary
> document `cloudferro-2024-ghg-report.pdf` is cached in this folder.

## 0. Operator confirmed: Netia (primary source)

CloudFerro's **own infrastructure page** states WAW3-1 and WAW3-2 are hosted in data
centres **operated by Netia**, not Atman ([cloudferro.com/cloud/data-centers](https://cloudferro.com/cloud/data-centers/)).
Press corroborates "CloudFerro is one of the first customers of the Netia data center
near Warsaw" ([DCD](https://www.datacenterdynamics.com/en/news/polands-netia-opens-data-center-near-warsaw/)).
Physical site: **Netia "MIND" data centre, Jawczyce** (commissioned 1 April 2021;
Sadowa 5) ([datacentermap](https://www.datacentermap.com/poland/warsaw/netia-mind/)).
See [[01-identity-location]] for the full identity adjudication.

**Supply chain:** CloudFerro S.A. (tenant) → **Netia** (colocation operator) → Polish
grid (PSE) + green-energy certificates.

## 1. PUE (Power Usage Effectiveness)

- **Netia (the actual host):** no published PUE for Jawczyce/MIND — "energy-efficient
  power and cooling" only ([datacentermap](https://www.datacentermap.com/poland/warsaw/netia-mind/)).
- **CloudFerro's *other* Polish site (Łódź/LCJ):** stated **PUE below 1.25**, "powered
  entirely by renewable energy" — a proxy for the class CloudFerro targets, but **not**
  WAW3 ([cloudnews.tech](https://cloudnews.tech/cloudferro-strengthens-its-commitment-to-the-european-sovereign-cloud-in-lodz/)).
  *Do not attribute this figure to WAW3.*
- **CNDCP benchmark for context:** new DCs in cool climates should hit annualised **PUE
  ≤ 1.3** ([CNDCP](https://www.climateneutraldatacentre.net/)).
- **Assessment: WAW3 PUE is undisclosed.**

## 2. Energy sourcing & renewable claims

- **CloudFerro:** claims "over 95 % renewable" and even "over 100 % of the energy we use
  comes from renewable sources" — the ">100 %" phrasing is a marketing red flag
  ([sustainability](https://cloudferro.com/sustainability/)).
- **Netia:** PPA-style supply deal with **Innogy Polska (now E.ON)** powering its
  Warsaw/Kraków DCs from **onshore wind** from Feb 2021; Jawczyce billed as "100 % green
  energy" ([DCD](https://www.datacenterdynamics.com/en/news/polands-netia-switches-to-renewable-energy-to-power-data-centers-and-cloud-services/);
  [datacenters.com](https://www.datacenters.com/netia-jawczyce)).

**Critical nuance — certificates vs physical grid.** All claims are **market-based**
(guarantees of origin / GO certificates), not on-site generation. Physically every
facility draws from the **Polish national grid (PSE)**, which is coal-dominated. The
renewable claim is an accounting overlay. CloudFerro's own GHG report quantifies the gap
(§5).

## 3. ESG disclosures & net-zero

- **CloudFerro:** annual **GHG Emissions Reports (2023/24/25)** via the Envirly platform
  to the GHG Protocol, all three scopes ([sustainability](https://cloudferro.com/sustainability/);
  [2024 report PDF](https://cloudferro.s3.waw3-2.cloudferro.com/wp-content/uploads/2026/04/CloudFerroS.A.-2024-2024-Greenhouse-Gas-Emissions-Report.pdf)).
  **No net-zero target date stated.**
- **None of Netia, CloudFerro, or Atman is a signatory of the Climate Neutral Data
  Centre Pact** — the only Polish operator on the register is **Beyond.pl** ([CNDCP register](https://www.climateneutraldatacentre.net/public-register/)).

## 4. Energy consumption & capacity

- **CloudFerro total electricity 2024: 12,650.4 MWh** (whole company) — 2024 GHG report p.7.
- **Netia Jawczyce/MIND:** ~1,000 m² server space, ~520 racks; backup **2 × DRUPS (Hitec
  PowerPRO2700) at 2 MW each** (~4 MW class); PLN 79 m build ([DCD](https://www.datacenterdynamics.com/en/news/polands-netia-opens-data-center-near-warsaw/)).
  No published annual MWh.

## 5. CO₂ emissions — the headline finding (CloudFerro 2024, read from the PDF)

| Metric (tCO₂eq unless noted) | Value |
|---|---|
| **Total emissions** | **11,602.4** |
| Scope 1 (combustion) | 20.5 |
| **Scope 2 — market-based** (with green certificates) | **75.3** |
| **Scope 2 — location-based** (actual grid) | **8,483.6** |
| Scope 3 (total) | 11,506.5 |
| — of which capital goods (servers/hardware/fixed assets) | 10,787 (93 % of total) |
| Energy consumption | 12,650.4 MWh |
| Emission per employee | 50.2 (≈231 employees implied) |

Source: [2024 GHG report PDF](https://cloudferro.s3.waw3-2.cloudferro.com/wp-content/uploads/2026/04/CloudFerroS.A.-2024-2024-Greenhouse-Gas-Emissions-Report.pdf)
(cached as `cloudferro-2024-ghg-report.pdf`).

**The central, quantified story:**
- Market-based Scope 2 = **75.3 t** (the number implied by "100 % renewable"), but
  location-based Scope 2 = **8,483.6 t** — a **~113× gap**, entirely an artefact of green
  certificates. The physical electricity is coal-heavy Polish grid power.
- Implied grid factor: 8,483.6 t ÷ 12,650.4 MWh ≈ **671 gCO₂/kWh** — squarely Polish-grid (§9).
- **Scope 3 ≈ 99 % of the footprint**, and within it the **embodied carbon of IT
  hardware / capital goods is 93 % of the total**. For an EO/AI compute provider, the
  carbon is in the silicon and the buildout, not the (certificate-offset) electricity.

## 6. Cooling, waste heat, water

- **Netia Jawczyce:** "energy-efficient power and cooling"; **DRUPS (diesel rotary UPS)**
  flywheel backup instead of battery UPS — a genuine efficiency/embodied-carbon plus
  (no lead-acid/Li battery bank) ([datacenters.com](https://www.datacenters.com/netia-jawczyce)).
  No WUE / waste-heat-reuse / free-cooling-hours published.
- **Climate context:** Warsaw's cool continental climate favours **free/economiser
  cooling** (plausibly 5,000–7,000+ hrs/yr) — but no operator publishes actual figures.
  A data gap to push on.

## 7. Backup generators

- **Netia (WAW3 host):** **2 × 2 MW DRUPS** (Hitec PowerPRO2700) — diesel-fuelled
  flywheel+diesel; **no HVO/biofuel** ([DCD](https://www.datacenterdynamics.com/en/news/polands-netia-opens-data-center-near-warsaw/)).
- All backup power across operators is **fossil diesel**; monthly test runs carry real
  unaccounted NOx/PM/CO₂.

## 8. EU regulation (the lever for verified numbers)

- **Energy Efficiency Directive (EED 2023, Art. 11/12) + Delegated Reg. (EU) 2024/1364:**
  mandatory annual reporting for any EU data centre with **installed IT power ≥ 500 kW**
  into the **European data-centre database** — energy, PUE, water, waste-heat, renewable
  share. First report (CY2023) due **15 Sep 2024**; thereafter **15 May** annually
  ([EC](https://energy.ec.europa.eu/topics/energy-efficiency/energy-efficiency-targets-directive-and-rules/energy-efficiency-directive/energy-performance-data-centres_en);
  [DCD](https://www.datacenterdynamics.com/en/news/european-energy-efficiency-directive-published-with-mandatory-data-center-reporting/)).
  Netia Jawczyce almost certainly exceeds the threshold and is legally obliged to report
  — **so PUE/WUE/waste-heat figures exist in the EU database even though absent from
  corporate sites.** Best lever for verified data. Commission published its first
  analysis of 2024 submissions July 2025.
- **EU Code of Conduct for Data Centres:** Atman participates; voluntary, not binding.

## 9. Poland grid carbon-intensity (numbers, by year & source)

| Figure | Year | Basis | Source |
|---|---|---|---|
| **662 gCO₂/kWh** | 2023 | generation, highest in EU | [Ember](https://ember-energy.org/countries-and-regions/poland/) |
| ~600+ gCO₂/kWh (highest in EU) | 2024–25 | generation | [EEA/Ember](https://www.eea.europa.eu/en/analysis/indicators/greenhouse-gas-emission-intensity-of-1) |
| 618 gCO₂eq/kWh | 2025 | lifecycle/consumption | [Nowtricity](https://www.nowtricity.com/country/poland/) |
| ~671 gCO₂/kWh (implied) | 2024 | from CloudFerro's own location-based Scope 2 ÷ MWh | this report |
| coal/lignite ~52 %, fossil ~66 % | 2025 | electricity mix | [lowcarbonpower](https://lowcarbonpower.org/region/Poland) |

**Poland has consistently the most carbon-intensive electricity grid in the EU**
(~600–660 gCO₂/kWh; EU avg ≈ 12 % coal vs Poland ≈ 52 %). Every "100 % renewable" claim
sits on top of this physical grid, bridged only by certificates.

## 10. Confidence / open questions

**Verified / high confidence:**
- CloudFerro WAW3 hosted by **Netia** (CloudFerro's own page).
- CloudFerro 2024: total **11,602 tCO₂eq**; **market-based Scope 2 = 75 t vs
  location-based = 8,484 t** (113× gap); Scope 3 ≈ 99 %, hardware 93 %; **12,650 MWh**
  (read directly from the PDF — strongest evidence here).
- "Renewable" claims are market-based certificates, not on-site generation; physical
  supply is the coal-heavy Polish grid (~600–670 gCO₂/kWh).
- Backup is fossil diesel; **no operator is a CNDCP signatory**.

**Greenwashing flags:**
- CloudFerro's "over 100 % renewable" wording — physically impossible; certificate
  accounting masking ~8,500 t location-based reality.
- Vendor-buzzword efficiency claims ("ArcticFlow," helium drives) substituted for
  measured PUE/WUE, which are conspicuously absent.
- The ≤1.25 PUE figure is **Łódź, not WAW3**.

**Best next steps:**
1. Pull the **EU data-centre database (EED Art. 12)** entries for Netia Jawczyce —
   legally-required PUE/WUE/waste-heat/renewable share. Authoritative, non-marketing.
2. Confirm Netia's Innogy/E.ON wind PPA is **additional** (new capacity) vs unbundled GOs.
3. CloudFerro/Netia **net-zero target dates** — none currently published.

## Source list
- https://cloudferro.com/cloud/data-centers/
- https://cloudferro.com/sustainability/
- https://cloudferro.s3.waw3-2.cloudferro.com/wp-content/uploads/2026/04/CloudFerroS.A.-2024-2024-Greenhouse-Gas-Emissions-Report.pdf
- https://www.datacenterdynamics.com/en/news/polands-netia-opens-data-center-near-warsaw/
- https://www.datacenterdynamics.com/en/news/polands-netia-switches-to-renewable-energy-to-power-data-centers-and-cloud-services/
- https://www.datacentermap.com/poland/warsaw/netia-mind/
- https://www.datacenters.com/netia-jawczyce
- https://cloudnews.tech/cloudferro-strengthens-its-commitment-to-the-european-sovereign-cloud-in-lodz/
- https://www.climateneutraldatacentre.net/public-register/
- https://energy.ec.europa.eu/topics/energy-efficiency/energy-efficiency-targets-directive-and-rules/energy-efficiency-directive/energy-performance-data-centres_en
- https://ember-energy.org/countries-and-regions/poland/
- https://www.nowtricity.com/country/poland/
- https://lowcarbonpower.org/region/Poland
