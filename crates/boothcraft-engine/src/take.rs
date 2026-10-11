//! A recording in progress: input audio mapped to the document's channels, converted to the
//! document's rate. Live conversion is linear (cheap, for display); `finish` resamples properly.

pub struct Take {
    in_rate: u32,
    in_channels: usize,
    doc_rate: u32,
    /// Per document channel, at the input rate.
    raw: Vec<Vec<f32>>,
    /// Per document channel, at the document rate (approximate, for live display).
    live: Vec<Vec<f32>>,
    live_pos: f64,
    /// Maximum length at the document rate (punch-in), if any.
    limit: Option<usize>,
    peaks: Vec<f32>,
}

impl Take {
    pub fn new(
        in_rate: u32,
        in_channels: usize,
        doc_rate: u32,
        doc_channels: usize,
        limit: Option<usize>,
    ) -> Take {
        Take {
            in_rate,
            in_channels: in_channels.max(1),
            doc_rate,
            raw: vec![Vec::new(); doc_channels.max(1)],
            live: vec![Vec::new(); doc_channels.max(1)],
            live_pos: 0.0,
            limit,
            peaks: vec![0.0; in_channels.max(1)],
        }
    }

    /// Add interleaved input samples. Input channel `i` feeds document channel `i`; a mono input
    /// feeds every channel, and extra input channels are ignored.
    pub fn push_interleaved(&mut self, data: &[f32]) {
        let ch = self.in_channels;
        for frame in data.chunks_exact(ch) {
            for (p, v) in self.peaks.iter_mut().zip(frame) {
                *p = p.max(v.abs());
            }
            for (dc, raw) in self.raw.iter_mut().enumerate() {
                raw.push(frame[dc.min(ch - 1)]);
            }
        }
        self.convert_live();
    }

    fn convert_live(&mut self) {
        let step = self.in_rate as f64 / self.doc_rate as f64;
        let available = self.raw[0].len();
        while self.live_pos + 1.0 < available as f64 && !self.is_full() {
            let i = self.live_pos.floor() as usize;
            let frac = (self.live_pos - i as f64) as f32;
            for (raw, live) in self.raw.iter().zip(self.live.iter_mut()) {
                live.push(raw[i] + (raw[i + 1] - raw[i]) * frac);
            }
            self.live_pos += step;
        }
    }

    /// True once a punch-in has recorded its full length.
    pub fn is_full(&self) -> bool {
        self.limit.is_some_and(|l| self.live[0].len() >= l)
    }

    /// Recorded audio so far at the document rate, one vector per document channel.
    pub fn live(&self) -> &[Vec<f32>] {
        &self.live
    }

    pub fn live_len(&self) -> usize {
        self.live[0].len()
    }

    /// Input peak per input channel since the last call.
    pub fn take_peaks(&mut self) -> Vec<f32> {
        let out = self.peaks.clone();
        self.peaks.iter_mut().for_each(|p| *p = 0.0);
        out
    }

    /// The final take at the document rate, resampled with the high-quality converter.
    pub fn finish(self) -> Vec<Vec<f32>> {
        let limit = self.limit;
        self.raw
            .into_iter()
            .map(|raw| {
                let mut v = if self.in_rate == self.doc_rate {
                    raw
                } else {
                    boothcraft_dsp::convert::resample(&raw, self.in_rate, self.doc_rate)
                };
                if let Some(l) = limit {
                    v.truncate(l);
                }
                v
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_rate_stereo_passes_through() {
        let mut t = Take::new(48_000, 2, 48_000, 2, None);
        t.push_interleaved(&[0.1, 0.2, 0.3, 0.4, 0.5, 0.6]);
        assert_eq!(t.live_len(), 2, "live lags one frame for interpolation");
        let out = t.finish();
        assert_eq!(out, vec![vec![0.1, 0.3, 0.5], vec![0.2, 0.4, 0.6]]);
    }

    #[test]
    fn mono_input_fills_stereo_and_peaks_reset() {
        let mut t = Take::new(48_000, 1, 48_000, 2, None);
        t.push_interleaved(&[0.5, -0.8, 0.1]);
        assert_eq!(t.take_peaks(), vec![0.8]);
        assert_eq!(t.take_peaks(), vec![0.0]);
        let out = t.finish();
        assert_eq!(out[0], out[1]);
    }

    #[test]
    fn rate_conversion_and_punch_limit() {
        let mut t = Take::new(44_100, 1, 48_000, 1, None);
        for _ in 0..10 {
            t.push_interleaved(&vec![0.25; 4410]);
        }
        let live = t.live_len();
        assert!((live as i64 - 48_000).abs() < 10, "live {live}");
        let out = t.finish();
        assert_eq!(out[0].len(), 48_000);

        let mut p = Take::new(48_000, 1, 48_000, 1, Some(1000));
        p.push_interleaved(&vec![0.1; 5000]);
        assert!(p.is_full());
        assert_eq!(p.live_len(), 1000);
        assert_eq!(p.finish()[0].len(), 1000);
    }
}
