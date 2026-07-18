-- fit the global energy monitor LNG terminals (GGIT, 2025-09 release) to the
-- s2e AOI schema: a FeatureCollection of one padded-envelope polygon per
-- EXPORT terminal, tagged with id/name (+ status, capacity).
--
-- dedup: GEM lists each liquefaction train/unit as its own feature, but all units
-- of a terminal share a ProjectID — so grouping by ProjectID collapses an N-train
-- terminal into ONE box (the bounding envelope of its units, padded ~2 km). that
-- is the whole point of doing this in SQL: the cli just reads `id`/`name`/geometry.
--
-- status filter keeps physically-built terminals (where a flare can exist); widen
-- the IN list to include 'proposed','cancelled','shelved' for every terminal.
--
-- run: duckdb < aoi/lng-terminals.sql   (writes aoi/lng-terminals.geojson)

INSTALL spatial; LOAD spatial;

COPY (
  SELECT ProjectID                                    AS id,
         any_value(TerminalName)                      AS "name",
         any_value(Status)                            AS status,
         round(sum(CapacityinMtpa), 2)                AS capacity_mtpa,
         ST_MakeEnvelope(min(Longitude) - 0.02, min(Latitude) - 0.02,
                         max(Longitude) + 0.02, max(Latitude) + 0.02) AS geom
  -- gem places prelude flng (T100000130339) ~165 km sse of the vessel's true browse
  -- basin mooring despite claiming "exact"; override with the vnf-derived position.
  FROM (SELECT * REPLACE (
          CASE ProjectID WHEN 'T100000130339' THEN 123.3158 ELSE Longitude END AS Longitude,
          CASE ProjectID WHEN 'T100000130339' THEN -13.7847 ELSE Latitude  END AS Latitude)
        FROM ST_Read('aoi/lng-terminals-2025-09.geojson'))
  WHERE FacilityType = 'export'
    AND Status IN ('operating', 'construction', 'idled', 'mothballed', 'retired')
  GROUP BY ProjectID
) TO 'aoi/lng-terminals.geojson'
  WITH (FORMAT GDAL, DRIVER 'GeoJSON', SRS 'EPSG:4326');
