//! Predator Lite library: protocol, controller, settings and Windows front end.
//! Kept separate from `main.rs` so unit tests run without the elevation manifest.
//!
//! Layers, bottom up:
//! - `protocol`: bit-level encodings of the BIOS WMI methods (pure, fully unit-tested).
//! - `wmi`: COM/WMI plumbing that sends them (`Backend` implementation, hotkey events).
//! - `controller`: turns `settings::Settings` into firmware calls; modes, rollback.
//! - `fan_curve`: the software fan curves of Balanced and Eco.
//! - `eco_limits`: Eco's Windows-side limits (power plan, nvidia-smi).
//! - `settings`: the settings file.
//! - `gui`: the Win32 window and tray icon; `status_report`: `--status`.
//! - `system`: small Windows helpers (System32 programs, power status).

pub mod controller;
pub mod eco_limits;
pub mod fan_curve;
pub mod gui;
pub mod protocol;
pub mod settings;
pub mod status_report;
pub mod system;
pub mod wmi;
