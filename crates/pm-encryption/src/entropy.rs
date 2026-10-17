//! Shannon entropy as Autopsy computes it
//! (`EncryptionDetectionTools.calculateEntropy`).

use pm_core::ReadAt;

/// Read size for the byte histogram.
const CHUNK: usize = 1024 * 1024;

/// Byte histogram of a whole source.
#[derive(Debug, Clone)]
pub struct Histogram {
    pub counts: [u64; 256],
    /// Bytes counted.
    pub total: u64,
}

impl Default for Histogram {
    fn default() -> Self {
        Self {
            counts: [0; 256],
            total: 0,
        }
    }
}

impl Histogram {
    /// Adds bytes to the histogram.
    #[allow(clippy::indexing_slicing, reason = "u8 index into a 256-entry table")]
    pub fn add(&mut self, data: &[u8]) {
        for &b in data {
            self.counts[usize::from(b)] += 1;
        }
        self.total += data.len() as u64;
    }

    /// Autopsy's entropy for content of `size` bytes: the probabilities
    /// divide by `size - 1` (Autopsy's off-by-one, kept for parity).
    pub fn autopsy_entropy(&self, size: u64) -> f64 {
        const ONE_OVER_LOG2: f64 = std::f64::consts::LOG2_E;
        let data_length = size as f64 - 1.0;
        let mut acc = 0.0f64;
        for &c in &self.counts {
            if c > 0 {
                let p = c as f64 / data_length;
                acc += p * p.ln() * ONE_OVER_LOG2;
            }
        }
        -acc
    }
}

/// Reads all of `src` into a histogram; `None` when `cancelled` returns
/// true (polled once per MiB) or the content cannot be read (Autopsy logs
/// and gives up).
pub fn histogram(src: &dyn ReadAt, cancelled: &dyn Fn() -> bool) -> Option<Histogram> {
    let size = src.size();
    let mut h = Histogram::default();
    let mut buf = vec![0u8; CHUNK.min(usize::try_from(size).unwrap_or(CHUNK)).max(1)];
    let mut off = 0u64;
    while off < size {
        if cancelled() {
            return None;
        }
        let want = usize::try_from((size - off).min(buf.len() as u64)).unwrap_or(0);
        let chunk = buf.get_mut(..want)?;
        match src.read_at(off, chunk) {
            Ok(0) => break,
            Ok(n) => {
                h.add(chunk.get(..n)?);
                off += n as u64;
            }
            Err(e) => {
                tracing::debug!("entropy: read error at {off}: {e}");
                return None;
            }
        }
    }
    Some(h)
}

/// Autopsy's entropy of `src` (`None`: cancelled or unreadable).
pub fn entropy(src: &dyn ReadAt, cancelled: &dyn Fn() -> bool) -> Option<f64> {
    histogram(src, cancelled).map(|h| h.autopsy_entropy(src.size()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_core::io::MemSource;

    #[test]
    fn uniform_and_constant() {
        // Every byte value equally often: log2(256) = 8 scaled by n/(n-1).
        let data: Vec<u8> = (0..256 * 64).map(|i| i as u8).collect();
        let n = data.len() as f64;
        let e = entropy(&MemSource::new(data), &|| false).unwrap();
        let want = -(n / (n - 1.0)) * (1.0 / (n - 1.0) * 64.0).log2();
        assert!((e - want).abs() < 1e-9, "{e} vs {want}");
        assert!(e > 7.99);
        let e = entropy(&MemSource::new(vec![7u8; 1000]), &|| false).unwrap();
        // One symbol: -(n/(n-1)) * log2(n/(n-1)), slightly below zero.
        assert!(e < 0.0 && e > -0.01, "{e}");
    }

    #[test]
    fn cancellation() {
        assert_eq!(entropy(&MemSource::new(vec![1u8; 10]), &|| true), None);
    }
}
