use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use rustfft::{num_complex::Complex, FftPlanner};

pub const FFT_SIZE: usize = 2048;

pub struct SpectrumState {
    pub num_bars: usize,
    pub smoothed: Vec<f32>,
    pub band_edges: Vec<usize>,
    pub sample_rate: u32,
}

impl SpectrumState {
    pub fn new(num_bars: usize, sample_rate: u32) -> Self {
        Self {
            num_bars,
            smoothed: baseline_bars(num_bars),
            band_edges: compute_band_edges(sample_rate, num_bars),
            sample_rate,
        }
    }

    pub fn ensure_bars(&mut self, num_bars: usize, sample_rate: u32) {
        if self.num_bars != num_bars || self.sample_rate != sample_rate {
            self.num_bars = num_bars;
            self.sample_rate = sample_rate;
            self.smoothed = baseline_bars(num_bars);
            self.band_edges = compute_band_edges(sample_rate, num_bars);
        }
    }

    /// Update smoothed spectrum from PCM buffer; holds snapshot when paused, shows gentle baseline in calm mode.
    pub fn compute_and_update(&mut self, spec_buf: &[f32], playing: bool, calm: bool) -> Vec<f32> {
        if calm {
            return baseline_bars(self.num_bars);
        }
        if !playing {
            // Freeze last active spectrum snapshot on pause (or subtle resting floor if idle)
            return self.smoothed.clone();
        }
        let target = compute_spectrum(spec_buf, &self.band_edges, self.num_bars);
        for (i, &val) in target.iter().enumerate() {
            if i >= self.smoothed.len() {
                break;
            }
            let cur = self.smoothed[i];
            self.smoothed[i] = if val >= cur {
                cur + (val - cur) * 0.65
            } else {
                cur + (val - cur) * 0.25
            };
        }
        self.smoothed.clone()
    }

    /// Render solid flat-topped spectrum bars with smooth 1/8-cell vertical resolution.
    pub fn render_lines(
        &self,
        normalized: &[f32],
        height: usize,
        color: (u8, u8, u8),
        double_wide: bool,
    ) -> Vec<Line<'static>> {
        const BLOCKS_WIDE: [&str; 9] = [
            "  ", "▁▁", "▂▂", "▃▃", "▄▄", "▅▅", "▆▆", "▇▇", "██",
        ];
        const BLOCKS_SINGLE: [&str; 9] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];

        let bar_style = Style::new().fg(Color::Rgb(color.0, color.1, color.2));
        let num_bars = self.num_bars;
        let height = height.max(1);
        let glyphs = if double_wide { &BLOCKS_WIDE } else { &BLOCKS_SINGLE };

        let mut lines = Vec::with_capacity(height);
        for row in (1..=height).rev() {
            let row_bottom = (row - 1) as f32;
            let mut spans = Vec::with_capacity(num_bars * 2);
            for i in 0..num_bars {
                if i > 0 {
                    spans.push(Span::raw(" "));
                }
                let val = normalized.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0);
                let frac = (val * height as f32 - row_bottom).clamp(0.0, 1.0);
                let idx = (frac * 8.0).round() as usize;
                let glyph = glyphs[idx.min(8)];
                if idx == 0 {
                    spans.push(Span::raw(glyph));
                } else {
                    spans.push(Span::styled(glyph, bar_style));
                }
            }
            lines.push(Line::from(spans));
        }
        lines
    }
}

fn baseline_bars(num_bars: usize) -> Vec<f32> {
    const PATTERN: [f32; 10] = [0.22, 0.22, 0.34, 0.92, 0.56, 0.80, 0.68, 0.34, 0.20, 0.08];
    (0..num_bars)
        .map(|i| {
            let idx = (i * PATTERN.len()) / num_bars.max(1);
            PATTERN[idx.min(PATTERN.len() - 1)]
        })
        .collect()
}

pub fn compute_band_edges(sample_rate: u32, num_bars: usize) -> Vec<usize> {
    let bin_hz = sample_rate.max(8000) as f64 / FFT_SIZE as f64;
    let bars = num_bars.max(1);
    let log_min = 60.0f64.log10();
    let log_max = 16000.0f64.log10();

    let mut edges: Vec<usize> = (0..=bars)
        .map(|i| {
            let t = i as f64 / bars as f64;
            let hz = 10f64.powf(log_min + t * (log_max - log_min));
            ((hz / bin_hz).round() as usize).clamp(1, FFT_SIZE / 2)
        })
        .collect();

    for i in 1..edges.len() {
        if edges[i] <= edges[i - 1] {
            edges[i] = (edges[i - 1] + 1).min(FFT_SIZE / 2);
        }
    }
    edges
}

pub fn compute_spectrum(spec_buf: &[f32], band_edges: &[usize], num_bars: usize) -> Vec<f32> {
    if spec_buf.is_empty() || band_edges.len() < num_bars + 1 {
        return vec![0.0; num_bars];
    }
    let len = spec_buf.len();
    let mut buffer: Vec<Complex<f32>> = spec_buf
        .iter()
        .enumerate()
        .map(|(i, &s)| {
            let w = 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / (len - 1).max(1) as f32).cos());
            Complex::new(s * w, 0.0)
        })
        .collect();

    let mut planner = FftPlanner::new();
    let fft = planner.plan_fft_forward(len);
    fft.process(&mut buffer);

    let half = &buffer[1..len / 2];
    if half.is_empty() {
        return vec![0.0; num_bars];
    }

    let mags: Vec<f32> = (0..num_bars)
        .map(|i| {
            let s = band_edges[i].min(half.len().saturating_sub(1));
            let e = band_edges[i + 1].min(half.len());
            if e > s {
                half[s..e].iter().map(|c| c.norm()).sum::<f32>() / (e - s) as f32
            } else {
                half[s].norm()
            }
        })
        .collect();

    let max_mag = mags.iter().copied().fold(0.0f32, f32::max);
    if max_mag < 1e-4 {
        return vec![0.0; num_bars];
    }
    let peak = max_mag.max(1e-6);
    mags.iter().map(|&m| (m / peak).clamp(0.0, 1.0)).collect()
}

/// Detect beat onset timestamps (seconds) from mono PCM at `sr`.
pub fn detect_beats(audio: &[f32], sr: u32) -> Vec<f64> {
    let hop = (0.05 * sr as f64) as usize;
    if hop == 0 || audio.len() < hop * 2 {
        return Vec::new();
    }
    let n = audio.len() / hop;
    let energy: Vec<f32> = (0..n)
        .map(|i| {
            let seg = &audio[i * hop..(i + 1) * hop];
            seg.iter().map(|&s| s * s).sum::<f32>() / hop as f32
        })
        .collect();
    let onset: Vec<f32> = energy.windows(2).map(|w| (w[1] - w[0]).max(0.0)).collect();
    let height = percentile(&onset, 68.0);
    let min_dist = (0.25 / 0.05) as usize;
    find_peaks(&onset, height, min_dist)
        .into_iter()
        .map(|i| i as f64 * 0.05)
        .collect()
}

/// Detect energetic drop timestamps (seconds) from mono PCM at `sr`.
pub fn detect_drops(audio: &[f32], sr: u32) -> Vec<f64> {
    const FRAME_SEC: f64 = 0.1;
    const SUPPRESS_SEC: f64 = 30.0;
    const MIN_DROP_SEC: f64 = 20.0;
    const MAX_DROPS: usize = 4;

    let frame_step = (FRAME_SEC * sr as f64) as usize;
    if frame_step == 0 || audio.is_empty() {
        return Vec::new();
    }
    let n = audio.len() / frame_step;
    let lookback = (2.0 * sr as f64) as usize;
    let ctx_back = (20.0 / FRAME_SEC) as usize;
    let ctx_front = (3.0 / FRAME_SEC) as usize;
    let sustain = (10.0 / FRAME_SEC) as usize;

    let lb: Vec<f64> = (0..n)
        .map(|fi| {
            let pos = fi * frame_step;
            let seg = &audio[pos.saturating_sub(lookback)..pos];
            if seg.is_empty() {
                return -90.0;
            }
            let rms = (seg.iter().map(|&s| (s * s) as f64).sum::<f64>() / seg.len() as f64).sqrt();
            20.0 * (rms + 1e-9).log10()
        })
        .collect();

    let long_rms = (audio.iter().map(|&s| (s * s) as f64).sum::<f64>() / audio.len() as f64).sqrt();
    let long_rms_db = 20.0 * (long_rms + 1e-9).log10();

    let mut drops = Vec::new();
    let mut last_drop = -SUPPRESS_SEC;

    for fi in 0..n {
        if drops.len() >= MAX_DROPS {
            break;
        }
        let ft = fi as f64 * FRAME_SEC;
        if ft < MIN_DROP_SEC {
            continue;
        }
        let cs = fi.saturating_sub(ctx_back);
        let ce = fi.saturating_sub(ctx_front);
        if ce <= cs {
            continue;
        }
        let lb_ctx = lb[cs..ce].iter().copied().fold(f64::INFINITY, f64::min);
        let lb_now = lb[fi];
        let fut_end = n.min(fi + sustain);
        let lb_fut = lb[fi..fut_end].iter().sum::<f64>() / (fut_end - fi).max(1) as f64;

        if lb_ctx < long_rms_db - 1.5
            && lb_now > long_rms_db
            && (lb_now - lb_ctx) > 2.0
            && lb_fut > long_rms_db
            && ft - last_drop > SUPPRESS_SEC
        {
            drops.push(ft);
            last_drop = ft;
        }
    }
    drops
}

fn find_peaks(x: &[f32], height: f32, distance: usize) -> Vec<usize> {
    let mut peaks = Vec::new();
    for i in 1..x.len().saturating_sub(1) {
        if x[i] > x[i - 1] && x[i] > x[i + 1] && x[i] >= height {
            if peaks.last().map_or(true, |&last| i - last >= distance) {
                peaks.push(i);
            }
        }
    }
    peaks
}

fn percentile(data: &[f32], p: f64) -> f32 {
    if data.is_empty() {
        return 0.0;
    }
    let mut sorted = data.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = (p / 100.0 * (sorted.len() - 1) as f64) as usize;
    sorted[idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_edges_are_increasing_and_bounded() {
        let edges = compute_band_edges(44100, 16);
        assert_eq!(edges.len(), 17);
        for i in 1..edges.len() {
            assert!(edges[i] >= edges[i - 1]);
            assert!(edges[i] <= FFT_SIZE / 2);
        }
    }

    #[test]
    fn spectrum_normalizes_sine_wave() {
        let edges = compute_band_edges(44100, 12);
        let buf: Vec<f32> = (0..FFT_SIZE).map(|i| (i as f32 * 0.15).sin()).collect();
        let vals = compute_spectrum(&buf, &edges, 12);
        assert_eq!(vals.len(), 12);
        assert!(vals.iter().all(|&v| (0.0..=1.0).contains(&v)));
        assert!(vals.iter().any(|&v| v > 0.5));
    }
}
