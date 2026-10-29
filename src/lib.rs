//! QODA çekirdek kütüphanesi: sinyal işleme ve DVB-S2 alıcı parçaları.
//! Arayüz ve cihaz kodu `main.rs` tarafında kalır.

pub mod detect;
pub mod dsp;
pub mod dvbs2;
pub mod sigmf;
pub mod tsinfo;
pub mod tuning;
