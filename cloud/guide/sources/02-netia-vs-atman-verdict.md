# Source 02 — Adjudication: Netia vs Atman (which building hosts WAW3-2)

> During research two strands disagreed on the host building. This file records the
> fact-check that settled it, because the name collision is itself a worthwhile detail
> for the article (it trips up most casual sources). See [[01-identity-location]].

## VERDICT: Netia "Data Center MIND", Jawczyce. Confidence: HIGH.

CloudFerro's WAW3-1/WAW3-2 regions physically sit in **Netia Data Center MIND, Sadowa 5,
Jawczyce (Ożarów Mazowiecki)**. The "Atman WAW-3" link was a coincidental name collision
with no supporting primary evidence, refuted by the timeline. CloudFerro's "WAW3" denotes
its *third Warsaw cloud generation* (region instances WAW3-1, WAW3-2), not Atman's WAW-3
*building*.

## Why the confusion arose

Two independent naming conventions collide:
- **Atman** numbers its *buildings*: WAW-1 (Grochowska), WAW-2 (Konstruktorska), WAW-3
  (Duchnice).
- **CloudFerro** numbers its *Warsaw cloud generations / OpenStack region instances*:
  WAW3-1, WAW3-2.

Both sit in **Ożarów Mazowiecki** west of Warsaw — Netia MIND in **Jawczyce**, Atman WAW-3
in **Duchnice**, ~3 km apart. Proximity + shared "WAW3" string makes the two easy to
conflate, and several directory sites do.

## Evidence FOR Netia (primary, decisive)

1. **Netia's own page names CloudFerro as anchor tenant** of its new DC: *"The first
   client who … uses the collocation and leased lines services, 'remote hands' services
   and rents additional office space is **CloudFerro** …"* — quotes CloudFerro's own
   Grzegorz Pawlicki: *"we were looking for another data center in Warsaw … scalable to
   at least 100 server racks."* ([Netia](https://www.netia.pl/en/operators/aktualnosci/netia-s-new-data-center-fills-up-at-a-record-pace)).
2. **CloudFerro/CREODIAS docs state the WAW3 regions are in Netia facilities** — WAW3-1
   and WAW3-2 both "hosted in data centers operated by **Netia** in Warsaw," sharing the
   one "WAW3 Data Center" ([CREODIAS](https://creodias.eu/cloud/cloudferro-cloud/);
   [cloudferro.com/cloud/data-centers](https://cloudferro.com/cloud/data-centers/)).
3. **Facility profile matches MIND** — PeeringDB fac/12758 "Netia Data Center MIND,"
   operator Netia S.A., Sadowa 5, Jawczyce ([PeeringDB](https://www.peeringdb.com/fac/12758));
   DCD corroborates the ~April 2021 opening ([DCD](https://www.datacenterdynamics.com/en/news/polands-netia-opens-data-center-near-warsaw/)).

## Evidence FOR Atman — and why it fails

1. **Only inferential support: the shared "WAW3" string.** No source asserts the link.
2. **Atman's own materials name no tenants and never mention CloudFerro**; the WAW-3
   grand opening was **18 September 2025** ([Atman](https://atman.pl/en/atman-opens-flagship-data-center-waw-3/)).

## The decisive timeline

CloudFerro WAW3-1/WAW3-2 reached **general availability in June 2023**. Atman WAW-3 opened
**18 September 2025** — over two years later. **A live, GA cloud region cannot have
launched in 2023 inside a building that opened in 2025.** Pre-lease, migration, and
split-site theories all lack any evidence, and current (2025–26) CloudFerro docs *still*
name Netia.

## Corporate note

Netia (Polsat Plus group) and Atman are separate, unrelated operators. Worth flagging for
the article: CloudFerro CEO **Maciej Krzyżanowski is a former CEO of ATM S.A.** — the
company historically behind the Atman brand. So an Atman tie is *plausible on its face*,
which is exactly why the name-collision needed a hard fact-check. The evidence still lands
on Netia for WAW3.

## What would make it incontrovertible
- A CloudFerro security/colocation/DPA disclosure naming "Netia DC MIND / Jawczyce".
- A CDSE infrastructure datasheet stating the WAW3 operator by name.
- A PeeringDB facility entry tying CloudFerro's org (AS200999) to fac/12758.

## Source list
- https://www.netia.pl/en/operators/aktualnosci/netia-s-new-data-center-fills-up-at-a-record-pace
- https://creodias.eu/cloud/cloudferro-cloud/
- https://cloudferro.com/cloud/data-centers/
- https://cloudferro.com/news/cloudferro-waw3-1-public-cloud-ga/
- https://cloudferro.com/news/cloudferro-waw3-2-and-fra1-2-regions-announced-for-creodias/
- https://www.peeringdb.com/fac/12758
- https://www.datacenterdynamics.com/en/news/polands-netia-opens-data-center-near-warsaw/
- https://atman.pl/en/atman-opens-flagship-data-center-waw-3/
- https://bgp.tools/as/200999
