//! Claude Governor Library
//!
//! Automated capacity governor for Claude Code subscription usage.

/// Authoritative claude-print adapter verification (claudego-b03e5c39):
/// invoke_template exit-0 probe + rule-3/rule-5 env-scrub check.
pub mod adapter_verify;
pub mod alerts;
pub mod burn_rate;
pub mod calibrator;
pub mod capacity_summary;
pub mod collector;
pub mod config;
pub mod db;
pub mod doctor;
pub mod governor;
/// Per-adapter verified-closure yield from the NEEDLE attempt ledger
/// (claudego-bba5584b).
pub mod ledger_yield;
pub mod narrator;
pub mod poller;
pub mod pricing;
pub mod schedule;
pub mod simulator;
pub mod snapshot_fixtures;
pub mod state;
pub mod status_display;
pub mod worker;
/// Session→worker attribution for collected usage records (claudego-a542d686).
pub mod worker_attribution;
