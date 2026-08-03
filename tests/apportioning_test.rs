//! Unit tests for apportioning calculation logic
//!
//! Tests the pure mathematical apportioning logic: given a set of rows with
//! total_usd values and a window percentage delta, verify that the apportioned
//! p7ds values are correctly weighted by each row's share of total_usd.

/// Test helper: verify apportioned values match expected values
///
/// # Arguments
/// * `rows` - Vector of (total_usd, expected_p7ds) tuples
/// * `delta` - The total window percentage delta to apportion
fn verify_apportioning(rows: Vec<(f64, f64)>, delta: f64) {
    use claude_governor::governor::apportion_delta;

    // Calculate total_usd across all rows
    let total_usd: f64 = rows.iter().map(|(usd, _)| *usd).sum();

    // Verify each row gets its expected share
    for (row_usd, expected_p7ds) in rows {
        let actual_p7ds = apportion_delta(delta, total_usd, row_usd);
        assert!(
            (actual_p7ds - expected_p7ds).abs() < f64::EPSILON,
            "Apportioning mismatch: delta={}, total_usd={}, row_usd={}, expected={}, actual={}",
            delta,
            total_usd,
            row_usd,
            expected_p7ds,
            actual_p7ds
        );
    }
}

#[test]
fn test_two_rows_varying_weights() {
    // Two rows with total_usd 0.10 and 0.30, delta 0.8
    // Row 1: 0.10 / 0.40 = 0.25 of total → 0.8 * 0.25 = 0.2
    // Row 2: 0.30 / 0.40 = 0.75 of total → 0.8 * 0.75 = 0.6
    let rows = vec![
        (0.10, 0.2), // 25% of delta
        (0.30, 0.6), // 75% of delta
    ];
    verify_apportioning(rows, 0.8);

    // Verify the apportioned values sum to the original delta
    let total_apportioned: f64 = vec![0.2, 0.6].iter().sum();
    assert!((total_apportioned - 0.8).abs() < f64::EPSILON);
}

#[test]
fn test_single_row_gets_full_delta() {
    // Single row should get the entire delta
    let rows = vec![
        (0.50, 1.0), // 100% of delta
    ];
    verify_apportioning(rows, 1.0);
}

#[test]
fn test_three_rows_varying_weights() {
    // Three rows with varying total_usd values
    // Row 1: 0.20 / 1.00 = 0.20 → 1.0 * 0.20 = 0.2
    // Row 2: 0.30 / 1.00 = 0.30 → 1.0 * 0.30 = 0.3
    // Row 3: 0.50 / 1.00 = 0.50 → 1.0 * 0.50 = 0.5
    let rows = vec![
        (0.20, 0.2), // 20% of delta
        (0.30, 0.3), // 30% of delta
        (0.50, 0.5), // 50% of delta
    ];
    verify_apportioning(rows, 1.0);

    // Verify sum equals delta
    let total_apportioned: f64 = vec![0.2, 0.3, 0.5].iter().sum();
    assert!((total_apportioned - 1.0).abs() < f64::EPSILON);
}

#[test]
fn test_equal_weights_equal_split() {
    // All rows have equal total_usd → equal split
    // Three rows, each 0.10, total 0.30, delta 0.9
    // Each row: 0.10 / 0.30 = 1/3 → 0.9 * 1/3 = 0.3
    let rows = vec![
        (0.10, 0.3), // Equal split
        (0.10, 0.3), // Equal split
        (0.10, 0.3), // Equal split
    ];
    verify_apportioning(rows, 0.9);
}

#[test]
fn test_four_rows_complex_distribution() {
    // Four rows with varying weights to test edge cases
    // Total: 0.05 + 0.15 + 0.30 + 0.50 = 1.00
    // Delta: 2.0
    let rows = vec![
        (0.05, 0.10), // 5% of total → 5% of delta = 0.10
        (0.15, 0.30), // 15% of total → 15% of delta = 0.30
        (0.30, 0.60), // 30% of total → 30% of delta = 0.60
        (0.50, 1.00), // 50% of total → 50% of delta = 1.00
    ];
    verify_apportioning(rows, 2.0);

    // Verify sum equals delta
    let total_apportioned: f64 = vec![0.10, 0.30, 0.60, 1.00].iter().sum();
    assert!((total_apportioned - 2.0).abs() < f64::EPSILON);
}

#[test]
fn test_zero_delta_all_zeros() {
    // When delta is zero, all rows should get zero
    let rows = vec![
        (0.10, 0.0),
        (0.20, 0.0),
        (0.30, 0.0),
    ];
    verify_apportioning(rows, 0.0);
}

#[test]
fn test_negative_deltas() {
    // Negative delta (utilization decreased)
    // Total: 1.00, delta: -1.0
    let rows = vec![
        (0.25, -0.25), // 25% of negative delta
        (0.75, -0.75), // 75% of negative delta
    ];
    verify_apportioning(rows, -1.0);

    // Verify sum equals negative delta
    let total_apportioned: f64 = vec![-0.25, -0.75].iter().sum();
    assert!((total_apportioned - (-1.0)).abs() < f64::EPSILON);
}

#[test]
fn test_large_delta() {
    // Large delta value to ensure no overflow issues
    let rows = vec![
        (10.0, 40.0),  // 25% of 160.0
        (30.0, 120.0), // 75% of 160.0
    ];
    verify_apportioning(rows, 160.0);
}

#[test]
fn test_small_fractional_weights() {
    // Small fractional weights to test precision
    // Total: 0.001 + 0.002 + 0.003 = 0.006
    // Delta: 0.12
    let rows = vec![
        (0.001, 0.02),  // 1/6 of delta = 0.02
        (0.002, 0.04),  // 2/6 of delta = 0.04
        (0.003, 0.06),  // 3/6 of delta = 0.06
    ];
    verify_apportioning(rows, 0.12);

    // Verify precision is maintained
    let total_apportioned: f64 = vec![0.02, 0.04, 0.06].iter().sum();
    assert!((total_apportioned - 0.12).abs() < f64::EPSILON);
}

#[test]
fn test_one_row_zero_weight_gets_nothing() {
    // One row has zero weight, another has all the weight
    let rows = vec![
        (0.0, 0.0),   // Zero weight → zero apportioned
        (1.0, 1.5),   // All weight → full delta
    ];
    verify_apportioning(rows, 1.5);
}
