//! QODA core library: signal processing and DVB-S2 receiver parts.
//! The UI and device code stay on the `main.rs` side.

pub mod detect;
pub mod dsp;
pub mod dvbs2;
pub mod sigmf;
pub mod tsinfo;
pub mod tuning;
