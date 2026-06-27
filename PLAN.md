# bulk-tile processing — shift the full-tile path off windowed range-reads

a short handoff plan. the gpu full-tile path works and is byte-parity with the cpu
path, but it is only ~1.1× faster end-to-end. profiling on the box (vm.l40s.1,
western-siberia, aug-2024) shows why — and points at a different architecture.

## what we measured (the evidence this plan rests on)

- one full tile, same 284 detections, cpu 12.7s vs gpu 11.5s → gpu buys ~1.1×.
- gpu utilisation ~0% throughout: nvjpeg2000 decode is a tiny slice of wall time.
- a whole B12 band is **7.3 MB**; a **direct sequential read is 0.12s** — the
  co-located store is fast, network is NOT the bottleneck.
- a **full-band gdal decode is ~2.36s** (≈2.2s decode + 0.12s fetch).
- yet a full-tile *detect* takes ~12s. the ~10s gap is **per-window JP2 decode
  overhead** — the detector reads hundreds of small windows (every B12 block, plus
  B11/B8A/SCL windows per hot block), and each windowed read re-does codestream /
  precinct decode. random-access into a compressed codestream is the slow thing,
  not the bytes and not the math.

conclusion: for bulk work, *windowed range-reads are the wrong access pattern*. the
fix is to touch more data but far more cheaply — read and decode each band **whole,
once**, then run detection entirely in memory.

## the change

a **bulk reader** for the full-tile / `--region` path (the windowed `GdalReader`
stays for point AOIs):

1. for each scene, fetch each band's whole codestream once (one sequential GET each,
   ~0.12s/band measured) — B12, B11, B8A, SCL.
2. decode each band **whole-tile**, not windowed:
   - gpu: nvjpeg2000 over all four bands (now the decode is the dominant step, and
     it's the step the GPU is good at — this is where batched decode finally pays).
   - cpu fallback: one gdal whole-band `RasterIO` per band (~2.2s each, but replaces
     the hundreds of windowed decodes, so still a net win).
3. hold all four bands resident in RAM (~4 × 5490² × 2B ≈ 240 MB/tile).
4. iterate `all_blocks` over the resident tiles, slice all four bands per block, call
   the unchanged `core::detect_block`. **zero per-block I/O.**

expected: per-tile ~12s → ~2–3s (fetch ~0.5s + decode + in-RAM detect); gpu
utilisation rises from ~0% to meaningful; throughput scales with concurrency until
GPU memory or decode bound (not I/O-latency bound).

the tradeoff is deliberate and is the whole point: we decode whole aux tiles even
where cold, touching more data — but bulk/global runs are flare-dense, and a single
whole-tile decode is far cheaper than the windowed overhead it replaces. (in sparse
single-site AOIs the windowed path is still better — keep it.)

## what stays fixed

- `core/` is untouched: still per-block slices in, detections out. parity holds
  because JP2 is lossless — whole-tile decode yields the same pixels as windowed.
- extend the parity gate: bulk reader (cpu and gpu) detections == windowed detections
  byte-for-byte over sample tiles. this is the hard guard against drift.
- the archive / cluster view / box.sh `archive`/`pull`/`publish` are unchanged — only
  the per-scene detection inner loop changes.

## seam

reuse the existing `SceneReader` trait. add a `BulkReader` (cpu whole-band RasterIO)
and fold the gpu path into it (all-band nvjpeg2000) — both produce candidates from
resident full-tile buffers via the shared `make_candidate`, then the existing
`detect_candidates` driver runs core. the windowed `GdalReader` is the AOI default;
`--region`/`--bulk` selects the bulk reader. connected-components stays on the CPU
(sparse → cheap), per the gpu-plan principle.

## validate, in order

1. ✓ floor confirmed: whole-band read 0.12s, full decode ~2.2s, windowed detect ~12s.
2. cpu bulk (whole-band RasterIO, in-RAM detect) vs windowed on one tile — expect a
   large drop from eliminating windowed-decode overhead, gpu not yet involved.
3. gpu bulk (all-band nvjpeg2000, batched) — expect decode to become the visible cost
   and gpu utilisation to climb.
4. parity: bulk == windowed, byte-for-byte.
5. throughput vs `--concurrency` and vs gpu memory; pick the operating point.

if step 2 alone already wins big, the GPU is optional — the real lever was the access
pattern, not the decoder. that is the hypothesis to settle first.
