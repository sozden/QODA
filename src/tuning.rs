//! LNB frequency calculations.

/// Computes the intermediate frequency (IF) at the LNB output. All values are in Hz.
///
/// For QO-100 reception the LNB local oscillator is below the signal frequency,
/// so IF = RF - LO.
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
