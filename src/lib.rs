//! QODA core library: signal processing, the DVB-S2 receiver and transmitter parts and the
//! libiio (AD9361) device layer shared by the receiver and transmitter programs.
//! The UI and the SoapySDR device code stay on the `main.rs` side.

pub mod detect;
pub mod dsp;
pub mod dvbs2;
pub mod ffmpeg;
pub mod iio;
pub mod sigmf;
pub mod tsinfo;
pub mod tuning;
