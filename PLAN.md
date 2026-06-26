# plan: port the bulk detector to cloudferro (eu-sovereign)

goal: run s2-flares bulk detection on a CloudFerro **WAW3-2** (warsaw) VM
co-located with the Copernicus `eodata` archive — off US infrastructure (see
memory `project_eu_sovereign_cloudferro`). you have an OpenStack `clouds.yaml`
(use it via `export OS_CLOUD=<name>` + the `openstack` CLI / python-openstackclient)
and ~$20 (≈€18) of credit — plenty: a full 16,592-scene run costs ~€0.14, so the
budget covers dev plus ~100 validation runs. use WAW3-2 **spot** for any burst and
**scale to zero** between runs; watch object-storage growth if you cache anything.

steps:
1. **provision** — `openstack` → keypair + a small `eo1.medium` (2/4) or `eo1.large`
   (4/8) VM in WAW3-2 with **EODATA access enabled** + a floating IP; ssh in;
   install node 22, gdal, duckdb.
2. **data** — generate `eodata` S3 keys; confirm **free in-region** windowed reads
   of `s3://eodata/Sentinel-2/...` via gdal `/vsis3/eodata/...`.
3. **stac** — repoint search at CDSE (`https://stac.dataspace.copernicus.eu/v1`,
   collection `sentinel-2-l2a`); asset hrefs are `.jp2`, not COG — adapt the band
   key/href shape in `lib/stac.js` (differs from Element84).
4. **reader (the hard part)** — geotiff.js can't read JP2. write a node-only
   `cog.js` sibling using **gdal-async** for windowed `/vsis3/eodata/...` JP2 reads
   that returns the same typed arrays `detect.js` expects. leave `detect.js` and the
   browser path untouched.
5. **fan-out** — replace the Lambda `Event` dispatch with a local parallel runner on
   the box (later: k8s Jobs, free control plane), preserving "PutObject == scene
   done, resumable" to a CloudFerro bucket.
6. **validate** — run one known AOI (e.g. a single LNG terminal) and diff detections
   against the existing AWS run before scaling; profile **read-vs-decode** to decide
   if a JP2→COG cache or GPU nvJPEG2000 decode is worth it (see memory).
