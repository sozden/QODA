//! Reading SigMF recordings (cf32_le and ci16_le).

use std::io;
use std::path::Path;

use num_complex::Complex;

pub struct Recording {
    pub sample_rate: f64,
    /// RF centre frequency of the recording (Hz); 0 if missing from the metadata.
    pub frequency: f64,
    pub samples: Vec<Complex<f32>>,
}

/// `path` may be the `.sigmf-meta`, the `.sigmf-data` or the base name without extension.
/// If `max_samples` is given, only that many leading samples are read.
pub fn read(path: &Path, max_samples: Option<usize>) -> io::Result<Recording> {
    let meta = std::fs::read_to_string(path.with_extension("sigmf-meta"))?;
    let invalid = |msg: &str| io::Error::new(io::ErrorKind::InvalidData, msg.to_string());
    let float = meta.contains("\"cf32_le\"");
    if !float && !meta.contains("\"ci16_le\"") {
        return Err(invalid("only cf32_le and ci16_le recordings are supported"));
    }
    let sample_rate = number(&meta, "core:sample_rate").ok_or_else(|| invalid("core:sample_rate is missing"))?;
    let frequency = number(&meta, "core:frequency").unwrap_or(0.0);

    let bytes = std::fs::read(path.with_extension("sigmf-data"))?;
    let mut samples: Vec<Complex<f32>> = if float {
        bytes
            .chunks_exact(8)
            .map(|c| {
                Complex::new(
                    f32::from_le_bytes([c[0], c[1], c[2], c[3]]),
                    f32::from_le_bytes([c[4], c[5], c[6], c[7]]),
                )
            })
            .collect()
    } else {
        // ci16: full scale ±32768.
        let scale = 1.0 / 32768.0;
        bytes
            .chunks_exact(4)
            .map(|c| {
                Complex::new(
                    i16::from_le_bytes([c[0], c[1]]) as f32 * scale,
                    i16::from_le_bytes([c[2], c[3]]) as f32 * scale,
                )
            })
            .collect()
    };
    if let Some(max) = max_samples {
        samples.truncate(max);
    }
    Ok(Recording { sample_rate, frequency, samples })
}

/// Finds the first number of the form `"key": number` in a JSON text.
/// Enough for simple metadata without needing a full JSON parser.
fn number(json: &str, key: &str) -> Option<f64> {
    let quoted = format!("\"{key}\"");
    let rest = &json[json.find(&quoted)? + quoted.len()..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E')))
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_numbers_from_meta() {
        let meta = r#"{ "global": { "core:sample_rate": 2400000, "core:datatype": "cf32_le" },
            "captures": [ { "core:frequency": 10491518750.32 } ] }"#;
        assert_eq!(number(meta, "core:sample_rate"), Some(2_400_000.0));
        assert_eq!(number(meta, "core:frequency"), Some(10_491_518_750.32));
        assert_eq!(number(meta, "core:missing"), None);
    }
}
