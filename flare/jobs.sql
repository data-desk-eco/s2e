-- flare's jobs from an s2e AOI and a `discover` manifest: every feature meets
-- every scene whose footprint touches it, as the feature's extent in the
-- scene's own UTM metres plus a halo (1 km by default, so a point site reads
-- the 2 km chip s2e reads). START, END and CLOUD narrow the manifest.
--   AOI=a.geojson SCENES=scenes.ndjson.gz duckdb -c '.read jobs.sql' | flare
install spatial;
load spatial;
.mode tabs
.headers off
with f as (
  select coalesce(f ->> '$.properties.id', 'f' || row_number() over ()) id,
         ST_GeomFromGeoJSON(f -> '$.geometry') g
  from (select unnest(features) f
        from read_json(getenv('AOI'), format = 'unstructured',
                       columns = {features: 'json[]'}))),
s as (
  select id, epsg, bbox, bands.b12 b12
  from read_json(getenv('SCENES'), format = 'newline_delimited',
                 columns = {id: 'varchar', date: 'varchar', epsg: 'int',
                            cloud_cover: 'double', bbox: 'double[4]',
                            bands: 'struct(b12 varchar)'})
  where bands.b12 is not null
    and date >= coalesce(nullif(getenv('START'), ''), '0')
    and date <= coalesce(nullif(getenv('END'), ''), '9')
    and coalesce(cloud_cover, 0)
        <= coalesce(nullif(getenv('CLOUD'), '')::double, 100)),
j as (
  select f.id || ':' || s.id key, b12,
         ST_Extent(ST_Transform(g, 'EPSG:4326', 'EPSG:' || epsg,
                                always_xy := true)) e,
         coalesce(nullif(getenv('HALO'), '')::double, 1000) h
  from f join s on ST_Intersects(ST_Extent(g),
    ST_MakeEnvelope(bbox[1], bbox[2], bbox[3], bbox[4])))
select key, b12, ST_XMin(e) - h, ST_YMin(e) - h, ST_XMax(e) + h,
       ST_YMax(e) + h
from j order by b12, key;
