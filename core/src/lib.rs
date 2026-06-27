//! the canonical sentinel-2 swir flare-detection methodology core. pure compute,
//! no i/o — slices in, detections/clusters out — so the one frozen methodology
//! drives the native gdal cli and the wasm browser/lambda shim alike. ported 1:1
//! from the js lineage (lib/), with the js unit tests carried over per module.
//!
//! the detect.js-takes-typed-arrays / io-in-cog.js boundary is the native-vs-wasm
//! seam: this crate is the "detect.js" half; i/o lives in the cli/wasm crates.

pub mod geo;
pub mod score;
pub mod detect;
pub mod cluster;
pub mod coverage;

// public surface mirroring lib/index.js.
pub use detect::{
    detect_block, dn_to_reflectance, screen_clouds, label_connected_components,
    enumerate_blocks, Block, BlockMeta, Detection, Thresholds, BLOCK_SIZE, BLOCK_OVERLAP,
};
pub use cluster::{cluster_detections, Cluster, ClusterOptions, DedupedDet};
pub use coverage::{cover_sites, CoverRow, Site};
pub use geo::{
    wgs84_to_utm, utm_to_wgs84, utm_params, epsg_from_mgrs, pad_bbox, bbox_area_km2,
    meters_to_degrees_lat, meters_to_degrees_lon,
};
pub use score::{
    score_cluster, ratio_score, persistence_score, glint_penalty, glint_suspect,
    glint_angle_nadir, glint_score_from_angle, glint_score_from_elevation, Score,
};
