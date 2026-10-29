//! LNB frekans hesapları.

/// LNB çıkışındaki ara frekansı (IF) hesaplar. Tüm değerler Hz cinsindendir.
///
/// QO-100 alımında LNB lokal osilatörü yayın frekansının altındadır,
/// bu yüzden IF = RF - LO olur.
pub fn intermediate_frequency(rf_hz: f64, lo_hz: f64) -> f64 {
    rf_hz - lo_hz
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a71a_on_9750_lnb() {
        let if_hz = intermediate_frequency(10_491.0e6, 9_750.0e6);
        assert_eq!(if_hz, 741.0e6);
    }
}
