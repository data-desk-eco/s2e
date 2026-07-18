//! cluster scoring — the vision-validated swir flare-quality methodology.
//! 1:1 port of lib/score.js (permian-flaring sql/30_score.sql, 2026-06-09 rebuild):
//!
//!   total_score = 0.50·ratio_score
//!               + 0.40·persistence_score·(0.1 + 0.9·ratio_score)
//!               − 0.40·min_glint_score
//!
//! every term is detection-intrinsic; brightness (peak b12) is a recall floor, not
//! a ranking term, so it is dropped. range −0.40 … +0.90.

// spectral ratio ramp 0→1 over [RATIO_FLOOR, RATIO_FLOOR + RATIO_SPAN] (= 1.1→1.7).
pub const RATIO_FLOOR: f64 = 1.1;
pub const RATIO_SPAN: f64 = 0.6;

pub const W_RATIO: f64 = 0.50;
pub const W_PERSIST: f64 = 0.40;
pub const PERSIST_FLOOR: f64 = 0.10;
pub const W_GLINT: f64 = 0.40;

// per-detection glint-suspect rule (parity with openflaring's glint_suspect).
pub const GLINT_SCORE_SUSPECT: f64 = 0.6;
pub const GLINT_RATIO_SUSPECT: f64 = 1.3;

/// angle between the specularly-reflected sun direction and a nadir view (s2 view
/// zenith ≤ 10° ⇒ treat as nadir ⇒ sun zenith = 90 − elevation).
pub fn glint_angle_nadir(sun_elevation_deg: f64) -> f64 {
    90.0 - sun_elevation_deg
}

/// map a glint angle to a 0–1 score (1 = high glint risk): 1.0 below 25°, fading
/// linearly to 0 at 65°.
pub fn glint_score_from_angle(glint_angle_deg: f64) -> f64 {
    let a = glint_angle_deg;
    if a <= 25.0 {
        1.0
    } else if a >= 65.0 {
        0.0
    } else {
        (65.0 - a) / 40.0
    }
}

/// glint_score straight from sun elevation (None when elevation is unknown).
pub fn glint_score_from_elevation(sun_elevation_deg: Option<f64>) -> Option<f64> {
    match sun_elevation_deg {
        Some(e) if !e.is_nan() => Some(glint_score_from_angle(glint_angle_nadir(e))),
        _ => None,
    }
}

/// ratio score in [0, 1]: a smooth ramp on the cluster's max b12/b11 ratio.
pub fn ratio_score(max_ratio: Option<f64>) -> f64 {
    match max_ratio {
        Some(r) if !r.is_nan() => ((r - RATIO_FLOOR) / RATIO_SPAN).clamp(0.0, 1.0),
        _ => 0.0,
    }
}

/// persistence score in [0, 1]: the clear-sky share lit, n_dates / n_obs.
pub fn persistence_score(n_dates: f64, n_obs: f64) -> f64 {
    if n_obs <= 0.0 {
        0.0
    } else {
        (n_dates / n_obs).min(1.0)
    }
}

/// geometric glint penalty in [−W_GLINT, 0], linear in the cluster's minimum
/// per-detection glint_score. js normalises −0 → +0 (the `|| 0`); match that.
pub fn glint_penalty(min_glint: Option<f64>) -> f64 {
    match min_glint {
        Some(g) if !g.is_nan() => {
            let v = -W_GLINT * g.clamp(0.0, 1.0);
            if v == 0.0 {
                0.0
            } else {
                v
            }
        }
        _ => 0.0,
    }
}

/// per-detection glint-suspect flag (high glint, flat ratio, single look).
pub fn glint_suspect(min_glint: Option<f64>, max_ratio: Option<f64>, n_dates: f64) -> bool {
    match min_glint {
        Some(g) if !g.is_nan() => {
            g >= GLINT_SCORE_SUSPECT
                && matches!(max_ratio, Some(r) if !r.is_nan() && r < GLINT_RATIO_SUSPECT)
                && n_dates <= 1.0
        }
        _ => false,
    }
}

/// the four score components for one cluster.
pub struct Score {
    pub ratio_score: f64,
    pub persistence_score: f64,
    pub glint_penalty: f64,
    pub total_score: f64,
}

/// score one cluster from its intrinsic aggregates.
pub fn score_cluster(
    max_ratio: Option<f64>,
    n_dates: f64,
    n_obs: f64,
    min_glint: Option<f64>,
) -> Score {
    let ratio_score = ratio_score(max_ratio);
    let persistence_score = persistence_score(n_dates, n_obs);
    let glint_penalty = glint_penalty(min_glint);
    let total_score = W_RATIO * ratio_score
        + W_PERSIST * persistence_score * (PERSIST_FLOOR + (1.0 - PERSIST_FLOOR) * ratio_score)
        + glint_penalty;
    Score {
        ratio_score,
        persistence_score,
        glint_penalty,
        total_score,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn ratio_ramp() {
        assert_eq!(ratio_score(Some(RATIO_FLOOR)), 0.0);
        assert_eq!(ratio_score(Some(0.5)), 0.0);
        assert!(approx(
            ratio_score(Some(RATIO_FLOOR + RATIO_SPAN / 2.0)),
            0.5
        ));
        assert!(approx(ratio_score(Some(RATIO_FLOOR + RATIO_SPAN)), 1.0));
        assert_eq!(ratio_score(Some(3.0)), 1.0);
        assert_eq!(ratio_score(None), 0.0);
    }
    #[test]
    fn persistence() {
        assert!(approx(persistence_score(5.0, 10.0), 0.5));
        assert_eq!(persistence_score(20.0, 10.0), 1.0);
        assert_eq!(persistence_score(3.0, 0.0), 0.0);
    }
    #[test]
    fn penalty() {
        assert!(approx(glint_penalty(Some(1.0)), -W_GLINT));
        assert!(glint_penalty(Some(0.0)).is_sign_positive()); // −0 normalised to +0
        assert_eq!(glint_penalty(Some(0.0)), 0.0);
        assert!(approx(glint_penalty(Some(0.5)), -0.2));
        assert_eq!(glint_penalty(None), 0.0);
    }
    #[test]
    fn cluster_totals() {
        let best = score_cluster(Some(2.0), 10.0, 10.0, Some(0.0));
        assert_eq!(best.ratio_score, 1.0);
        assert_eq!(best.persistence_score, 1.0);
        assert!(approx(best.total_score, W_RATIO + W_PERSIST));
        assert!(best.total_score < 1.0);
        let glinty = score_cluster(Some(1.1), 1.0, 0.0, Some(1.0));
        assert!(approx(glinty.total_score, -W_GLINT));
        let dim = score_cluster(Some(1.1), 10.0, 10.0, Some(0.0));
        assert!(approx(dim.total_score, W_PERSIST * 0.1));
    }
    #[test]
    fn glint_geometry() {
        assert_eq!(glint_score_from_elevation(Some(80.0)), Some(1.0));
        assert_eq!(glint_score_from_elevation(Some(10.0)), Some(0.0));
        assert_eq!(glint_score_from_elevation(None), None);
    }
}
