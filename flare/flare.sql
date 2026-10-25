-- flare at query time: search CDSE's catalogue and detect, in one query.
--   duckdb -unsigned -init flare/flare.sql
--   from s2_flares_in(-104.2, 31.6, -103.2, 32.6, '2025-05-01', '2025-05-31');
-- eodata hrefs need eodata credentials (AWS_ACCESS_KEY_ID etc.); away from
-- cloudferro, href := 'gcp' reads google's public mirror of the same files.
load spatial;
load flare;

-- every L1C scene over a lon/lat box between two dates, with the box in the
-- scene's own UTM metres clipped to its tile. the catalogue is asked a month
-- at a time, which returns at most 1000 scenes a month (about 60 MGRS tiles)
create or replace macro s2_scenes(w, s, e, n, t0, t1, href := 'eodata')
as table
select id, datetime, cloud, b12, crs,
       greatest(ST_XMin(x), pb[1]) xmin, greatest(ST_YMin(x), pb[2]) ymin,
       least(ST_XMax(x), pb[3]) xmax, least(ST_YMax(x), pb[4]) ymax
from (
  select f ->> '$.id' id,
         (f ->> '$.properties.datetime')::timestamp datetime,
         (f ->> '$.properties.eo:cloud_cover')::double cloud,
         if(href = 'gcp', regexp_replace(f ->> '$.assets.B12.href',
           '^s3://eodata/Sentinel-2/MSI/L1C/\d+/\d+/\d+/'
           || '(S2\w+_T(\d\d)(\w)(\w\w)_\w+\.SAFE)/',
           'https://storage.googleapis.com/gcp-public-data-sentinel-2/tiles/'
           || '\2/\3/\4/\1/'), f ->> '$.assets.B12.href') b12,
         f ->> '$.assets.B12.proj:code' crs,
         (f -> '$.assets.B12.proj:bbox')::double[] pb,
         ST_Extent(ST_Transform(ST_MakeEnvelope(w, s, e, n), 'EPSG:4326',
                                f ->> '$.assets.B12.proj:code',
                                always_xy := true)) x
  from (select unnest(features) f from read_json(
    list_transform(
      generate_series(date_trunc('month', t0::date), t1::date,
                      interval 1 month),
      lambda m: 'https://stac.dataspace.copernicus.eu/v1/search'
        || '?collections=sentinel-2-l1c&limit=1000'
        || '&bbox=' || w || ',' || s || ',' || e || ',' || n
        || '&datetime=' || strftime(greatest(m, t0::date), '%Y-%m-%d')
        || 'T00:00:00Z/' || strftime(least(m + interval 1 month
                                            - interval 1 day, t1::date),
                                      '%Y-%m-%d') || 'T23:59:59Z'
        || '&fields=id,properties.datetime,properties.eo:cloud_cover,'
        || 'assets.B12.href,assets.B12.proj:code,assets.B12.proj:bbox'),
    format = 'unstructured', columns = {features: 'json[]'})))
where xmax > xmin and ymax > ymin;

-- the flares in a lon/lat box between two dates, one row each. tiles of one
-- UTM zone share a pixel grid, so a flare in the overlap of two tiles of a
-- pass is the same pixel twice, and is kept once
create or replace macro s2_flares_in(w, s, e, n, t0, t1, max_cloud := 100,
                                     href := 'eodata', thresholds := '')
as table
select id scene, datetime, ST_X(p) lon, ST_Y(p) lat, clear, max_b12,
       avg_b12, peak_b11, ratio, peakedness, pixels, radiance, saturated
from (
  select *, ST_Transform(ST_Point(x, y), crs, 'EPSG:4326',
                         always_xy := true) p
  from (select id, datetime, crs, r.clear,
               unnest(r.flares, recursive := true)
        from (select *, s2_flares(b12, xmin, ymin, xmax, ymax, thresholds) r
              from s2_scenes(w, s, e, n, t0, t1, href := href)
              where cloud <= max_cloud)))
where ST_X(p) between w and e and ST_Y(p) between s and n
qualify row_number() over (partition by datetime, crs, x, y order by id) = 1;
