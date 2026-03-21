//! Prediction Accuracy Self-Calibration
//!
//! This module provides automatic calibration of governor prediction parameters
//! by scoring past predictions against actual outcomes at window reset events.
//!
//! ## How It Works
//!
//! 1. When a window resets, the calibrator records the actual utilization change
//! 2. It compares this to the predicted change (based on burn rate at prediction time)
//! 3. It computes error statistics and auto-tunes parameters:
//!    - `alpha`: EMA smoothing factor (higher = more responsive)
//!    - `hysteresis`: Minimum change threshold for scaling actions
//!    - `target_utilization`: Adjusted based on systematic bias
//!
//! ## Data Storage
//!
//! Prediction accuracy scores are stored in `prediction-accuracy.jsonl`:
//! ```json
//! {"ts":"2026-03-20T10:00:00Z","win":"seven_day_sonnet","predicted":5.2,"actual":4.8,"error":-0.4,"pct_error":-8.3}
//! ```

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Minimum samples required before auto-tuning kicks in
const MIN_SAMPLES_FOR_TUNING: u32 = 10;

/// Alpha clamp range (smoothing factor)
const ALPHA_MIN: f64 = 0.1;
const ALPHA_MAX: f64 = 0.5;

/// Hysteresis clamp range (percentage points)
const HYSTERESIS_MIN: f64 = 0.5;
const HYSTERESIS_MAX: f64 = 3.0;

/// Target utilization adjustment clamp
const TARGET_UTIL_ADJUST_MAX: f64 = 5.0;

/// Default alpha value
pub const DEFAULT_ALPHA: f64 = 0.25;

/// Default hysteresis value
pub const DEFAULT_HYSTERESIS: f64 = 1.0;

// ---------------------------------------------------------------------------
// Data Types
// ---------------------------------------------------------------------------

/// A single prediction accuracy score
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PredictionScore {
    /// ISO 8601 timestamp when the prediction was made
    pub ts: DateTime<Utc>,

    /// Window name (five_hour, seven_day, seven_day_sonnet)
    pub win: String,

    /// Predicted utilization change (percentage points)
    pub predicted: f64,

    /// Actual utilization change (percentage points)
    pub actual: f64,

    /// Error (actual - predicted)
    pub error: f64,

    /// Percentage error (error / actual * 100)
    pub pct_error: f64,
}

/// Aggregated calibration statistics
#[derive(Debug, Clone, Default)]
pub struct CalibrationStats {
    /// Total number of predictions scored
    pub total_samples: u32,

    /// Samples per window
    pub samples_by_window: std::collections::HashMap<String, u32>,

    /// Mean error across all predictions
    pub mean_error: f64,

    /// Median error
    pub median_error: f64,

    /// Standard deviation of errors
    pub stddev_error: f64,

    /// Mean absolute percentage error (MAPE)
    pub mape: f64,

    /// Median error for seven_day_sonnet specifically
    pub median_error_7ds: f64,

    /// Systematic bias indicator:
    /// - Positive: consistently under-predicting (actual > predicted)
    /// - Negative: consistently over-predicting (actual < predicted)
    pub bias: f64,
}

/// Auto-tuned parameters from calibration
#[derive(Debug, Clone)]
pub struct TunedParams {
    /// EMA smoothing factor (higher = more responsive to new data)
    pub alpha: f64,

    /// Hysteresis threshold for scaling decisions
    pub hysteresis: f64,

    /// Suggested target utilization adjustment
    pub target_util_adjustment: f64,

    /// Whether auto-tuning was applied (false if not enough samples)
    pub tuned: bool,
}

// ---------------------------------------------------------------------------
// Calibration Logic
// ---------------------------------------------------------------------------

/// Score a prediction against actual outcome
///
/// Creates a PredictionScore record that can be appended to the accuracy log.
pub fn score_prediction(
    window: &str,
    predicted_change: f64,
    actual_change: f64,
    prediction_time: DateTime<Utc>,
) -> PredictionScore {
    let error = actual_change - predicted_change;
    let pct_error = if actual_change.abs() > 1e-9 {
        (error / actual_change.abs()) * 100.0
    } else if predicted_change.abs() > 1e-9 {
        // Actual is zero but predicted non-zero: 100% error
        100.0 * error.signum()
    } else {
        0.0
    };

    PredictionScore {
        ts: prediction_time,
        win: window.to_string(),
        predicted: predicted_change,
        actual: actual_change,
        error,
        pct_error,
    }
}

/// Compute calibration statistics from a list of scores
pub fn compute_stats(scores: &[PredictionScore]) -> CalibrationStats {
    if scores.is_empty() {
        return CalibrationStats::default();
    }

    let mut stats = CalibrationStats::default();
    stats.total_samples = scores.len() as u32;

    // Count by window
    for score in scores {
        *stats
            .samples_by_window
            .entry(score.win.clone())
            .or_insert(0) += 1;
    }

    // Compute mean error
    let sum: f64 = scores.iter().map(|s| s.error).sum();
    stats.mean_error = sum / scores.len() as f64;

    // Compute median error
    let mut errors: Vec<f64> = scores.iter().map(|s| s.error).collect();
    errors.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    stats.median_error = if errors.len() % 2 == 0 {
        (errors[errors.len() / 2 - 1] + errors[errors.len() / 2]) / 2.0
    } else {
        errors[errors.len() / 2]
    };

    // Compute median error for seven_day_sonnet specifically
    let errors_7ds: Vec<f64> = scores
        .iter()
        .filter(|s| s.win == "seven_day_sonnet")
        .map(|s| s.error)
        .collect();
    if !errors_7ds.is_empty() {
        let mut sorted = errors_7ds.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        stats.median_error_7ds = if sorted.len() % 2 == 0 {
            (sorted[sorted.len() / 2 - 1] + sorted[sorted.len() / 2]) / 2.0
        } else {
            sorted[sorted.len() / 2]
        };
    }

    // Compute standard deviation
    let variance: f64 = scores
        .iter()
        .map(|s| (s.error - stats.mean_error).powi(2))
        .sum::<f64>()
        / scores.len() as f64;
    stats.stddev_error = variance.sqrt();

    // Compute MAPE (mean absolute percentage error)
    let abs_pct_sum: f64 = scores.iter().map(|s| s.pct_error.abs()).sum();
    stats.mape = abs_pct_sum / scores.len() as f64;

    // Determine bias
    // If mean error is significantly different from zero, we have a bias
    if stats.mean_error.abs() > stats.stddev_error * 0.5 {
        stats.bias = stats.mean_error.signum();
    } else {
        stats.bias = 0.0;
    }

    stats
}

/// Auto-tune parameters based on calibration statistics
///
/// Tuning rules:
/// - If consistently under-predicting (bias > 0): increase alpha (more responsive)
/// - If consistently over-predicting (bias < 0): decrease alpha (more stable)
/// - High variance: increase hysteresis (require bigger changes before acting)
/// - Low variance: decrease hysteresis (can act on smaller changes)
pub fn auto_tune(stats: &CalibrationStats, current_alpha: f64, current_hysteresis: f64) -> TunedParams {
    if stats.total_samples < MIN_SAMPLES_FOR_TUNING {
        return TunedParams {
            alpha: current_alpha,
            hysteresis: current_hysteresis,
            target_util_adjustment: 0.0,
            tuned: false,
        };
    }

    // Adjust alpha based on bias
    let alpha_adjustment = match stats.bias {
        b if b > 0.0 => 0.05, // Under-predicting: be more responsive
        b if b < 0.0 => -0.05, // Over-predicting: be more stable
        _ => 0.0,
    };
    let new_alpha = (current_alpha + alpha_adjustment).clamp(ALPHA_MIN, ALPHA_MAX);

    // Adjust hysteresis based on variance
    // High variance (stddev > 2 * |mean|) -> increase hysteresis
    // Low variance (stddev < 0.5 * |mean|) -> decrease hysteresis
    let hysteresis_adjustment = if stats.stddev_error > stats.mean_error.abs() * 2.0 {
        0.25 // High variance: require bigger changes
    } else if stats.stddev_error < stats.mean_error.abs() * 0.5 {
        -0.25 // Low variance: can act on smaller changes
    } else {
        0.0
    };
    let new_hysteresis = (current_hysteresis + hysteresis_adjustment).clamp(HYSTERESIS_MIN, HYSTERESIS_MAX);

    // Suggest target utilization adjustment based on systematic bias
    // If we're consistently under-predicting, the target might be too aggressive
    let target_util_adjustment = if stats.bias.abs() > 0.0 {
        // Adjust in the direction that would compensate for the bias
        // If under-predicting (bias > 0), raise target slightly
        // If over-predicting (bias < 0), lower target slightly
        (stats.mean_error * 0.1).clamp(-TARGET_UTIL_ADJUST_MAX, TARGET_UTIL_ADJUST_MAX)
    } else {
        0.0
    };

    TunedParams {
        alpha: new_alpha,
        hysteresis: new_hysteresis,
        target_util_adjustment,
        tuned: true,
    }
}

// ---------------------------------------------------------------------------
// JSONL Storage
// ---------------------------------------------------------------------------

/// Default path for the prediction accuracy log
pub fn default_accuracy_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".needle")
        .join("state")
        .join("prediction-accuracy.jsonl")
}

/// Append a prediction score to the accuracy log
pub fn append_score(score: &PredictionScore) -> std::io::Result<()> {
    append_score_to_path(score, &default_accuracy_path())
}

/// Append a prediction score to a specific path
pub fn append_score_to_path(score: &PredictionScore, path: &PathBuf) -> std::io::Result<()> {
    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Open file for append (create if doesn't exist)
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;

    // Serialize and write as a single line
    let json = serde_json::to_string(score)?;
    writeln!(file, "{}", json)?;

    Ok(())
}

/// Read all prediction scores from the accuracy log
pub fn read_all_scores() -> std::io::Result<Vec<PredictionScore>> {
    read_all_scores_from_path(&default_accuracy_path())
}

/// Read all prediction scores from a specific path
pub fn read_all_scores_from_path(path: &PathBuf) -> std::io::Result<Vec<PredictionScore>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let file = File::open(path)?;
    let reader = BufReader::new(file);

    let scores: Vec<PredictionScore> = reader
        .lines()
        .filter_map(|line| {
            line.ok().and_then(|l| {
                serde_json::from_str::<PredictionScore>(&l).ok()
            })
        })
        .collect();

    Ok(scores)
}

/// Read the last N prediction scores
pub fn read_last_scores(n: usize) -> std::io::Result<Vec<PredictionScore>> {
    read_last_scores_from_path(n, &default_accuracy_path())
}

/// Read the last N prediction scores from a specific path
pub fn read_last_scores_from_path(n: usize, path: &PathBuf) -> std::io::Result<Vec<PredictionScore>> {
    let all_scores = read_all_scores_from_path(path)?;

    let start = if all_scores.len() > n {
        all_scores.len() - n
    } else {
        0
    };

    Ok(all_scores[start..].to_vec())
}

/// Compute current calibration from the accuracy log
pub fn compute_current_calibration() -> std::io::Result<CalibrationStats> {
    let scores = read_all_scores()?;
    Ok(compute_stats(&scores))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // --- Score computation tests ---

    #[test]
    fn score_prediction_computes_error_correctly() {
        let now = Utc::now();
        let score = score_prediction("seven_day_sonnet", 5.0, 4.5, now);

        assert_eq!(score.win, "seven_day_sonnet");
        assert!((score.predicted - 5.0).abs() < 1e-9);
        assert!((score.actual - 4.5).abs() < 1e-9);
        assert!((score.error - (-0.5)).abs() < 1e-9); // actual - predicted
    }

    #[test]
    fn score_prediction_computes_pct_error() {
        let now = Utc::now();

        // 5.0 predicted, 4.0 actual: error = -1.0, pct = -25%
        let score = score_prediction("five_hour", 5.0, 4.0, now);
        assert!((score.error - (-1.0)).abs() < 1e-9);
        assert!((score.pct_error - (-25.0)).abs() < 1e-6);

        // 2.0 predicted, 3.0 actual: error = 1.0, pct = 33.33%
        let score2 = score_prediction("five_hour", 2.0, 3.0, now);
        assert!((score2.error - 1.0).abs() < 1e-9);
        assert!((score2.pct_error - 33.333).abs() < 0.01);
    }

    #[test]
    fn score_prediction_handles_zero_actual() {
        let now = Utc::now();

        // Predicted 5, actual 0: -100% error (over-predicted)
        let score = score_prediction("five_hour", 5.0, 0.0, now);
        assert!((score.error - (-5.0)).abs() < 1e-9);
        assert!((score.pct_error - (-100.0)).abs() < 1e-9);

        // Both zero: 0% error
        let score2 = score_prediction("five_hour", 0.0, 0.0, now);
        assert!((score2.pct_error - 0.0).abs() < 1e-9);
    }

    // --- Stats computation tests ---

    #[test]
    fn compute_stats_empty_returns_default() {
        let stats = compute_stats(&[]);
        assert_eq!(stats.total_samples, 0);
        assert!((stats.mean_error - 0.0).abs() < 1e-9);
    }

    #[test]
    fn compute_stats_single_sample() {
        let scores = vec![PredictionScore {
            ts: Utc::now(),
            win: "seven_day_sonnet".to_string(),
            predicted: 5.0,
            actual: 4.0,
            error: -1.0,
            pct_error: -25.0,
        }];

        let stats = compute_stats(&scores);
        assert_eq!(stats.total_samples, 1);
        assert!((stats.mean_error - (-1.0)).abs() < 1e-9);
        assert!((stats.median_error - (-1.0)).abs() < 1e-9);
        assert!((stats.stddev_error - 0.0).abs() < 1e-9);
    }

    #[test]
    fn compute_stats_multiple_samples() {
        let scores = vec![
            PredictionScore {
                ts: Utc::now(),
                win: "seven_day_sonnet".to_string(),
                predicted: 5.0,
                actual: 4.0,
                error: -1.0,
                pct_error: -25.0,
            },
            PredictionScore {
                ts: Utc::now(),
                win: "seven_day_sonnet".to_string(),
                predicted: 3.0,
                actual: 4.0,
                error: 1.0,
                pct_error: 25.0,
            },
            PredictionScore {
                ts: Utc::now(),
                win: "seven_day_sonnet".to_string(),
                predicted: 2.0,
                actual: 3.0,
                error: 1.0,
                pct_error: 33.33,
            },
        ];

        let stats = compute_stats(&scores);
        assert_eq!(stats.total_samples, 3);
        assert_eq!(stats.samples_by_window.get("seven_day_sonnet"), Some(&3));

        // Mean error = (-1 + 1 + 1) / 3 = 0.333
        assert!((stats.mean_error - 0.333).abs() < 0.01);

        // Median of [-1, 1, 1] = 1.0
        assert!((stats.median_error - 1.0).abs() < 1e-9);

        // MAPE = (25 + 25 + 33.33) / 3 = 27.78
        assert!((stats.mape - 27.78).abs() < 0.5);
    }

    #[test]
    fn compute_stats_detects_bias() {
        // Consistently under-predicting (actual > predicted)
        let scores: Vec<PredictionScore> = (0..5)
            .map(|i| PredictionScore {
                ts: Utc::now(),
                win: "seven_day_sonnet".to_string(),
                predicted: 3.0,
                actual: 5.0 + i as f64 * 0.1,
                error: 2.0 + i as f64 * 0.1,
                pct_error: 40.0,
            })
            .collect();

        let stats = compute_stats(&scores);
        assert!(stats.bias > 0.0, "Should detect positive bias (under-predicting)");
    }

    #[test]
    fn compute_stats_7ds_median() {
        let scores = vec![
            PredictionScore {
                ts: Utc::now(),
                win: "five_hour".to_string(),
                predicted: 5.0,
                actual: 5.0,
                error: 0.0,
                pct_error: 0.0,
            },
            PredictionScore {
                ts: Utc::now(),
                win: "seven_day_sonnet".to_string(),
                predicted: 3.0,
                actual: 4.0,
                error: 1.0,
                pct_error: 25.0,
            },
            PredictionScore {
                ts: Utc::now(),
                win: "seven_day_sonnet".to_string(),
                predicted: 2.0,
                actual: 3.5,
                error: 1.5,
                pct_error: 42.86,
            },
        ];

        let stats = compute_stats(&scores);
        // Median of [1.0, 1.5] for 7ds = 1.25
        assert!((stats.median_error_7ds - 1.25).abs() < 1e-9);
    }

    // --- Auto-tune tests ---

    #[test]
    fn auto_tune_requires_minimum_samples() {
        let stats = CalibrationStats {
            total_samples: 5,
            ..Default::default()
        };

        let tuned = auto_tune(&stats, DEFAULT_ALPHA, DEFAULT_HYSTERESIS);
        assert!(!tuned.tuned);
        assert!((tuned.alpha - DEFAULT_ALPHA).abs() < 1e-9);
    }

    #[test]
    fn auto_tune_increases_alpha_when_under_predicting() {
        let stats = CalibrationStats {
            total_samples: 20,
            mean_error: 2.0,  // Positive = under-predicting
            stddev_error: 1.0,
            bias: 1.0,
            ..Default::default()
        };

        let tuned = auto_tune(&stats, DEFAULT_ALPHA, DEFAULT_HYSTERESIS);
        assert!(tuned.tuned);
        assert!(tuned.alpha > DEFAULT_ALPHA, "Alpha should increase when under-predicting");
    }

    #[test]
    fn auto_tune_decreases_alpha_when_over_predicting() {
        let stats = CalibrationStats {
            total_samples: 20,
            mean_error: -2.0, // Negative = over-predicting
            stddev_error: 1.0,
            bias: -1.0,
            ..Default::default()
        };

        let tuned = auto_tune(&stats, DEFAULT_ALPHA, DEFAULT_HYSTERESIS);
        assert!(tuned.tuned);
        assert!(tuned.alpha < DEFAULT_ALPHA, "Alpha should decrease when over-predicting");
    }

    #[test]
    fn auto_tune_increases_hysteresis_with_high_variance() {
        let stats = CalibrationStats {
            total_samples: 20,
            mean_error: 1.0,
            stddev_error: 5.0, // High variance (> 2 * |mean|)
            ..Default::default()
        };

        let tuned = auto_tune(&stats, DEFAULT_ALPHA, DEFAULT_HYSTERESIS);
        assert!(tuned.hysteresis > DEFAULT_HYSTERESIS, "Hysteresis should increase with high variance");
    }

    #[test]
    fn auto_tune_decreases_hysteresis_with_low_variance() {
        let stats = CalibrationStats {
            total_samples: 20,
            mean_error: 2.0,
            stddev_error: 0.5, // Low variance (< 0.5 * |mean|)
            ..Default::default()
        };

        let tuned = auto_tune(&stats, DEFAULT_ALPHA, DEFAULT_HYSTERESIS);
        assert!(tuned.hysteresis < DEFAULT_HYSTERESIS, "Hysteresis should decrease with low variance");
    }

    #[test]
    fn auto_tune_clamps_alpha() {
        let stats = CalibrationStats {
            total_samples: 20,
            mean_error: 10.0, // Strong under-prediction
            stddev_error: 1.0,
            bias: 1.0,
            ..Default::default()
        };

        // Start at max alpha
        let tuned = auto_tune(&stats, ALPHA_MAX, DEFAULT_HYSTERESIS);
        assert!((tuned.alpha - ALPHA_MAX).abs() < 1e-9, "Alpha should be clamped to max");

        // Start at min alpha with over-prediction
        let stats2 = CalibrationStats {
            total_samples: 20,
            mean_error: -10.0,
            stddev_error: 1.0,
            bias: -1.0,
            ..Default::default()
        };
        let tuned2 = auto_tune(&stats2, ALPHA_MIN, DEFAULT_HYSTERESIS);
        assert!((tuned2.alpha - ALPHA_MIN).abs() < 1e-9, "Alpha should be clamped to min");
    }

    #[test]
    fn auto_tune_clamps_hysteresis() {
        let stats = CalibrationStats {
            total_samples: 20,
            mean_error: 1.0,
            stddev_error: 100.0, // Extreme variance
            ..Default::default()
        };

        // Start at max hysteresis
        let tuned = auto_tune(&stats, DEFAULT_ALPHA, HYSTERESIS_MAX);
        assert!((tuned.hysteresis - HYSTERESIS_MAX).abs() < 1e-9, "Hysteresis should be clamped to max");
    }

    #[test]
    fn auto_tune_suggests_target_util_adjustment() {
        let stats = CalibrationStats {
            total_samples: 20,
            mean_error: 3.0, // Consistent under-prediction
            stddev_error: 1.0,
            bias: 1.0,
            ..Default::default()
        };

        let tuned = auto_tune(&stats, DEFAULT_ALPHA, DEFAULT_HYSTERESIS);
        // Adjustment should be in direction to compensate for bias
        assert!(tuned.target_util_adjustment > 0.0, "Should suggest raising target util");
        assert!(tuned.target_util_adjustment <= TARGET_UTIL_ADJUST_MAX);
    }

    // --- JSONL storage tests ---

    #[test]
    fn jsonl_append_and_read() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("test-accuracy.jsonl");

        let score = PredictionScore {
            ts: Utc::now(),
            win: "seven_day_sonnet".to_string(),
            predicted: 5.0,
            actual: 4.5,
            error: -0.5,
            pct_error: -11.11,
        };

        append_score_to_path(&score, &path).unwrap();

        let loaded = read_all_scores_from_path(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert!((loaded[0].predicted - 5.0).abs() < 1e-9);
        assert!((loaded[0].actual - 4.5).abs() < 1e-9);
    }

    #[test]
    fn jsonl_multiple_scores() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("test-accuracy.jsonl");

        for i in 0..5 {
            let score = PredictionScore {
                ts: Utc::now(),
                win: "seven_day_sonnet".to_string(),
                predicted: i as f64,
                actual: (i + 1) as f64,
                error: 1.0,
                pct_error: 20.0,
            };
            append_score_to_path(&score, &path).unwrap();
        }

        let loaded = read_all_scores_from_path(&path).unwrap();
        assert_eq!(loaded.len(), 5);

        let last_3 = read_last_scores_from_path(3, &path).unwrap();
        assert_eq!(last_3.len(), 3);
    }

    #[test]
    fn jsonl_format_is_valid() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("test-accuracy.jsonl");

        let score = PredictionScore {
            ts: "2026-03-20T10:00:00Z".parse().unwrap(),
            win: "seven_day_sonnet".to_string(),
            predicted: 5.0,
            actual: 4.5,
            error: -0.5,
            pct_error: -11.11,
        };

        append_score_to_path(&score, &path).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1);

        let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed.get("win").unwrap(), "seven_day_sonnet");
        assert_eq!(parsed.get("predicted").unwrap(), 5.0);
    }

    #[test]
    fn read_from_nonexistent_returns_empty() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("nonexistent.jsonl");

        let loaded = read_all_scores_from_path(&path).unwrap();
        assert!(loaded.is_empty());
    }
}
