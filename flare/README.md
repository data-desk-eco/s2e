# flare

A Sentinel-2 L1C flare detector in C. It reads the JPEG2000 archive
directly and runs as a command or as a DuckDB extension. With the
extension, one SQL query searches the CDSE catalogue and detects flares
in a region. No stored detections are necessary.

```sql
-- duckdb -unsigned -init flare/flare.sql
from s2_flares_in(-104.2, 31.6, -103.2, 32.6, '2025-05-01', '2025-05-31',
                  max_cloud := 50);
```

## Build

You need a C compiler, `pkg-config`, libcurl and OpenJPEG 2.5 or later.

```bash
make -C flare            # flare and flare.duckdb_extension
make -C flare install    # so that `load flare` works in any directory
```

The extension uses the stable DuckDB C API (v1.2.0). It loads in DuckDB
1.2 or later. It is not signed, so start DuckDB with `-unsigned`.

## Use from DuckDB

`flare.sql` loads the extension and the spatial extension, and defines
two macros:

- `s2_scenes(w, s, e, n, t0, t1, href := 'eodata')` gives each L1C scene
  that touches a longitude/latitude box between two dates. Each scene has
  its B12 href, its UTM zone, and the box in the metres of that zone.
- `s2_flares_in(w, s, e, n, t0, t1, max_cloud := 100, href := 'eodata',
  thresholds := '')` gives one row for each flare, with lon, lat and the
  detection values.

The macros ask the CDSE catalogue for one month at a time. One month
gives a maximum of 1000 scenes, which is approximately 60 MGRS tiles.

`eodata` hrefs need the eodata credentials in `AWS_ACCESS_KEY_ID` and
`AWS_SECRET_ACCESS_KEY`. Away from CloudFerro, `href := 'gcp'` reads the
same files from the public Google Cloud mirror.

The function below the macros is:

```sql
s2_flares(b12 VARCHAR, xmin DOUBLE, ymin DOUBLE, xmax DOUBLE, ymax DOUBLE
          [, thresholds VARCHAR])
  -> STRUCT(clear DOUBLE, error VARCHAR, flares STRUCT(x, y, max_b12,
            avg_b12, peak_b11, ratio, peakedness, pixels, radiance,
            saturated)[])
```

The box is in metres of the UTM zone of the scene. `clear` is the
cloud-free fraction of the observed pixels in the box. It is NULL when
the tile has no data in the box. A failed row has an `error` value, and
the query continues. `thresholds` is `'name=value,...'`. Run `flare
--help` to see the names and the defaults.

## Use from the shell

```bash
AOI=aoi.geojson SCENES=scenes.ndjson.gz duckdb -c '.read flare/jobs.sql' |
  flare/flare > flares.tsv
```

The input has one job on each line: `key`, B12 URL, `xmin`, `ymin`,
`xmax`, `ymax`, separated by tabs. `jobs.sql` makes these lines from an
s2e AOI and a `s2e discover` manifest. The output is tab-separated with a
header. Each detection has one row. A job without detections has one row
with empty detection columns. A failed job has the status `err`, and its
reason goes to stderr. To run the failed jobs again, select them by
status.

`FLARE_THREADS` sets the number of workers (default 64).
`FLARE_CACHE` sets the number of cache slots (default 4096).

## How it reads the archive

Sentinel-2 JP2 files before baseline 05.12 have no tile index. GDAL finds
tile *k* with *k* + 1 sequential range requests. In a test from a home
connection, a 100-pixel window took 2, 42 and 82 requests at three
positions in the file. flare does these steps:

1. It reads the 16 KB header, which gives the tiling and the GMLJP2
   georeferencing.
2. It reads the cloud mask (a few KB). It does not fetch JP2 tiles that
   are below cloud.
3. It finds each tile-part from a guessed offset. The byte pair 0xFF90
   cannot occur in the coded data, so a scan for the SOT marker finds
   the tile-part. The offsets of the tile-parts change little between
   dates, so flare learns them for each MGRS tile and band. After it
   learns them, it reads a tile in approximately 1.1 requests. If a box
   needs more than a quarter of a band, flare reads the full codestream
   in one request.
4. The main header and one tile-part make a complete codestream.
   OpenJPEG decodes only the box from it.
5. It reads B11 and B8A only around the candidate flares in B12.
6. It keeps headers, masks and tile-parts in a cache, so other sites on
   the same scene, and the same query with new thresholds, do not send
   requests again.

## Method

The thresholds are the s2e compact-source defaults. flare examines the
whole box in one pass:

- It calculates the background median for each 320-pixel cell, from a
  histogram of the B12 counts.
- It calculates the spectral mask and its 4-connected components.
- It applies the morphological gates.
- It grows the combustion-hot core from the peak of each component.

These are the differences from the s2e block detector:

- There are no overlapping blocks. Components that cross a cell edge are
  joined.
- Pixels with no data are not part of the background.
- Cloud comes from the L1C `MSK_CLASSI_B00` mask (opaque cloud and
  cirrus). CloudSEN is not used.

A numpy and scipy implementation of the same method was run on bands
that GDAL decoded. On one full tile, it gave the same 87 detections as
the command and the extension, to ten significant figures.

## Measurements

These are from a laptop on a home connection, reading the Google Cloud
mirror. In-region reads on CloudFerro are not yet measured.

- One full tile from local files: 5.1 s on one thread. The JPEG2000
  decode uses most of this time.
- 30 by 22 km in the Permian, March to June 2025, from the macro: 46
  scenes, 78 flares, 14 s.
- 100 by 110 km in the Permian, May 2025: 41 scenes, 521 flares, 66 s.
  The connection limited this run (61 s of CPU time on approximately one
  core).

## Limits

- flare needs baseline N0400 or later, because older products have no
  raster cloud mask.
- A flare in the overlap of two MGRS tiles in the same UTM zone is the
  same pixel in each tile, so the macro removes the duplicate. Across a
  zone boundary, the two detections stay.
- Signed `s3://` reads and the Linux build with Ubuntu's OpenJPEG 2.5.0
  are not yet tested.
