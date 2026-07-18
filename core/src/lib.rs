//! the canonical sentinel-2 swir flare-detection methodology core. pure compute,
//! no i/o — slices in, detections/clusters out — so the one frozen methodology
//! drives the native gdal cli and the wasm browser/lambda shim alike. ported 1:1
//! from the js lineage (lib/), with the js unit tests carried over per module.
//!
//! the detect.js-takes-typed-arrays / io-in-cog.js boundary is the native-vs-wasm
//! seam: this crate is the "detect.js" half; i/o lives in the cli/wasm crates.

pub mod cluster;
pub mod coverage;
pub mod detect;
pub mod geo;
pub mod plume;
pub mod score;

// public surface mirroring lib/index.js.
pub use cluster::{cluster_detections, Cluster, ClusterOptions, DedupedDet};
pub use coverage::{cell_key, cover_sites, grid_sites, snap, CoverRow, Site, CLEAR_MAX, GRID_STEP};
pub use detect::{
    calibrate_dn, detect_block, dn_to_reflectance, enumerate_blocks, label_connected_components,
    screen_clouds, Block, BlockMeta, Detection, Thresholds, BLOCK_OVERLAP, BLOCK_SIZE,
};
pub use geo::{
    bbox_area_km2, epsg_from_mgrs, meters_to_degrees_lat, meters_to_degrees_lon, pad_bbox,
    utm_params, utm_to_wgs84, wgs84_to_utm,
};
pub use score::{
    glint_angle_nadir, glint_penalty, glint_score_from_angle, glint_score_from_elevation,
    glint_suspect, persistence_score, ratio_score, score_cluster, Score,
};
