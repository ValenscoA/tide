use std::collections::HashMap;

use image::{imageops::FilterType, DynamicImage};
use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use ratatui_image::{picker::Picker, protocol::StatefulProtocol};

#[rustfmt::skip]
const BAYER: [[f32; 8]; 8] = [
    [ 0./64., 32./64.,  8./64., 40./64.,  2./64., 34./64., 10./64., 42./64.],
    [48./64., 16./64., 56./64., 24./64., 50./64., 18./64., 58./64., 26./64.],
    [12./64., 44./64.,  4./64., 36./64., 14./64., 46./64.,  6./64., 38./64.],
    [60./64., 28./64., 52./64., 20./64., 62./64., 30./64., 54./64., 22./64.],
    [ 3./64., 35./64., 11./64., 43./64.,  1./64., 33./64.,  9./64., 41./64.],
    [51./64., 19./64., 59./64., 27./64., 49./64., 17./64., 57./64., 25./64.],
    [15./64., 47./64.,  7./64., 39./64., 13./64., 45./64.,  5./64., 37./64.],
    [63./64., 31./64., 55./64., 23./64., 61./64., 29./64., 53./64., 21./64.],
];

const BRAILLE_DOTS: [(usize, usize, u32); 8] = [
    (0, 0, 0x01),
    (1, 0, 0x02),
    (2, 0, 0x04),
    (3, 0, 0x40),
    (0, 1, 0x08),
    (1, 1, 0x10),
    (2, 1, 0x20),
    (3, 1, 0x80),
];

pub struct CachedCover {
    pub palette: Vec<(u8, u8, u8)>,
    pub protocol: StatefulProtocol,
    pub mini_braille: Vec<Line<'static>>,
    pub player_braille: Vec<Line<'static>>,
}

pub struct CoverManager {
    pub picker: Picker,
    pub cache: HashMap<String, CachedCover>,
}

impl CoverManager {
    pub fn new_stdio() -> Self {
        let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::from_fontsize((10, 20)));
        Self {
            picker,
            cache: HashMap::new(),
        }
    }

    pub fn new_headless() -> Self {
        Self {
            picker: Picker::from_fontsize((10, 20)),
            cache: HashMap::new(),
        }
    }

    pub fn insert_bytes(&mut self, key: &str, bytes: &[u8]) -> Option<Vec<(u8, u8, u8)>> {
        let img = image::load_from_memory(bytes).ok()?;
        let palette = extract_palette(&img);
        let protocol = self.picker.new_resize_protocol(img.clone());
        let mini_braille = render_braille_lines(&img, 10);
        let player_braille = render_braille_lines(&img, 22);
        if self.cache.len() > 64 {
            if let Some(first_key) = self.cache.keys().next().cloned() {
                self.cache.remove(&first_key);
            }
        }
        self.cache.insert(
            key.to_string(),
            CachedCover {
                palette: palette.clone(),
                protocol,
                mini_braille,
                player_braille,
            },
        );
        Some(palette)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut CachedCover> {
        self.cache.get_mut(key)
    }

    pub fn has(&self, key: &str) -> bool {
        self.cache.contains_key(key)
    }
}

/// Render a subtle dim wave box (`art_chars` wide × `art_chars/2` tall) when no cover is loaded.
pub fn render_wave_placeholder(art_chars: usize) -> Vec<Line<'static>> {
    let char_h = (art_chars / 2).max(4);
    let dim = Style::new().fg(Color::DarkGray);
    let mut lines = Vec::with_capacity(char_h);
    for row in 0..char_h {
        let mut s = String::with_capacity(art_chars);
        for col in 0..art_chars {
            let ch = if row == 0 || row + 1 == char_h {
                if col == 0 || col + 1 == art_chars { '·' } else { '─' }
            } else if col == 0 || col + 1 == art_chars {
                '│'
            } else if row == char_h / 2 && (col == art_chars / 2 - 1 || col == art_chars / 2) {
                if col == art_chars / 2 - 1 { '♪' } else { ' ' }
            } else {
                ' '
            };
            s.push(ch);
        }
        lines.push(Line::styled(s, dim));
    }
    lines
}

pub fn extract_palette(img: &DynamicImage) -> Vec<(u8, u8, u8)> {
    let img_rgb = img.resize_exact(40, 40, FilterType::Lanczos3).to_rgb8();
    let rgba: Vec<u8> = img_rgb
        .pixels()
        .flat_map(|p| [p[0], p[1], p[2], 255u8])
        .collect();
    let nq = color_quant::NeuQuant::new(10, 16, &rgba);
    let pal_raw = nq.color_map_rgba();
    vibrant_palette(pal_raw.chunks(4).map(|c| (c[0], c[1], c[2])).collect(), 3)
}

fn vibrant_palette(candidates: Vec<(u8, u8, u8)>, n: usize) -> Vec<(u8, u8, u8)> {
    let mut scored: Vec<((u8, u8, u8), f32)> = candidates
        .into_iter()
        .filter_map(|(r, g, b)| {
            let rf = r as f32 / 255.0;
            let gf = g as f32 / 255.0;
            let bf = b as f32 / 255.0;
            let max = rf.max(gf).max(bf);
            let min = rf.min(gf).min(bf);
            let luma = 0.299 * rf + 0.587 * gf + 0.114 * bf;
            let sat = if max > 0.0 { (max - min) / max } else { 0.0 };
            if luma < 0.08 || luma > 0.94 || sat < 0.15 {
                return None;
            }
            let brightness_bonus = 1.0 - (luma - 0.55).abs() * 1.5;
            Some(((r, g, b), sat * brightness_bonus.max(0.1)))
        })
        .collect();

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut result: Vec<(u8, u8, u8)> = scored.into_iter().map(|(c, _)| c).collect();
    result.truncate(n);
    while result.len() < n {
        result.push((0, 229, 255));
    }
    result
}

pub fn render_braille_lines(img: &DynamicImage, art_chars: usize) -> Vec<Line<'static>> {
    let px_w = art_chars * 2;
    let px_h = art_chars * 2;
    let img_rgb = img
        .resize_exact(px_w as u32, px_h as u32, FilterType::Lanczos3)
        .to_rgb8();

    let gray_dyn = DynamicImage::ImageRgb8(img_rgb.clone()).to_luma8();
    let (gw, gh) = gray_dyn.dimensions();
    let avg: f32 = gray_dyn.pixels().map(|p| p[0] as f32).sum::<f32>() / (gw * gh).max(1) as f32;
    let enhanced: Vec<f32> = gray_dyn
        .pixels()
        .map(|p| (avg + 1.5 * (p[0] as f32 - avg)).clamp(0.0, 255.0) / 255.0)
        .collect();

    let mut pixels = vec![false; px_w * px_h];
    for py in 0..gh as usize {
        for px in 0..gw as usize {
            let v = enhanced[py * gw as usize + px];
            pixels[py * gw as usize + px] = v > BAYER[py % 8][px % 8];
        }
    }

    let char_h = px_h / 4;
    let mut color_lines = Vec::with_capacity(char_h);
    for char_row in 0..char_h {
        let mut color_spans = Vec::with_capacity(art_chars);
        for char_col in 0..art_chars {
            let mut val: u32 = 0;
            for &(dr, dc, bit) in &BRAILLE_DOTS {
                let pr = char_row * 4 + dr;
                let pc = char_col * 2 + dc;
                if pr < px_h && pc < px_w && pixels[pr * px_w + pc] {
                    val |= bit;
                }
            }
            let ch = char::from_u32(0x2800 + val).unwrap_or(' ');
            let pr0 = char_row * 4;
            let pc0 = char_col * 2;
            let (mut rs, mut gs, mut bs, mut count) = (0u32, 0u32, 0u32, 0u32);
            for dr in 0..4 {
                for dc in 0..2 {
                    let pr = (pr0 + dr) as u32;
                    let pc = (pc0 + dc) as u32;
                    if pr < gh && pc < gw {
                        let p = img_rgb.get_pixel(pc, pr);
                        rs += p[0] as u32;
                        gs += p[1] as u32;
                        bs += p[2] as u32;
                        count += 1;
                    }
                }
            }
            let (r, g, b) = if count > 0 {
                ((rs / count) as u8, (gs / count) as u8, (bs / count) as u8)
            } else {
                (255, 255, 255)
            };
            color_spans.push(Span::styled(
                ch.to_string(),
                Style::new().fg(Color::Rgb(r, g, b)),
            ));
        }
        color_lines.push(Line::from(color_spans));
    }
    color_lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};

    #[test]
    fn extracts_vibrant_palette_and_braille() {
        let img: DynamicImage =
            ImageBuffer::from_pixel(32, 32, Rgb([230u8, 45u8, 90u8])).into();
        let pal = extract_palette(&img);
        assert_eq!(pal.len(), 3);
        assert!(pal[0].0 > 150);

        let lines = render_braille_lines(&img, 8);
        assert_eq!(lines.len(), 4);
    }
}
