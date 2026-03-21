//! Claude Governor Library
//!
//! Automated capacity governor for Claude Code subscription usage.

pub mod alerts;
pub mod burn_rate;
pub mod calibrator;
pub mod capacity_summary;
pub mod collector;
pub mod config;
pub mod db;
pub mod doctor;
pub mod governor;
pub mod narrator;
pub mod poller;
pub mod pricing;
pub mod schedule;
pub mod simulator;
pub mod state;
pub mod status_display;
pub mod worker;
