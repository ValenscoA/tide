use std::collections::HashMap;
use std::path::Path;

use ratatui::style::Color;

use crate::config::{omarchy_theme_path, ThemeMode};

#[derive(Debug, Clone)]
pub struct Theme {
    pub bg: Color,
    pub surface: Color,
    pub fg: Color,
    pub muted: Color,
    pub accent: Color,
    pub accent_secondary: Color,
    pub selection_bg: Color,
    pub selection_fg: Color,
    pub border: Color,
    pub border_focused: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
}

impl Default for Theme {
    fn default() -> Self {
        dark_theme()
    }
}

pub fn dark_theme() -> Theme {
    Theme {
        bg: Color::Rgb(15, 17, 21),
        surface: Color::Rgb(22, 26, 33),
        fg: Color::Rgb(234, 238, 244),
        muted: Color::Rgb(118, 127, 144),
        accent: Color::Rgb(0, 229, 255),
        accent_secondary: Color::Rgb(166, 130, 255),
        selection_bg: Color::Rgb(38, 46, 61),
        selection_fg: Color::Rgb(255, 255, 255),
        border: Color::Rgb(56, 64, 80),
        border_focused: Color::Rgb(0, 229, 255),
        success: Color::Rgb(90, 214, 145),
        warning: Color::Rgb(242, 194, 92),
        error: Color::Rgb(244, 96, 108),
    }
}

pub fn parse_hex_color(s: &str) -> Option<(u8, u8, u8)> {
    let s = s.trim().trim_matches('"').trim_matches('\'').trim_start_matches('#');
    if s.len() < 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some((r, g, b))
}

fn to_color(rgb: (u8, u8, u8)) -> Color {
    Color::Rgb(rgb.0, rgb.1, rgb.2)
}

fn lighten(rgb: (u8, u8, u8), amount: i16) -> (u8, u8, u8) {
    (
        (rgb.0 as i16 + amount).clamp(0, 255) as u8,
        (rgb.1 as i16 + amount).clamp(0, 255) as u8,
        (rgb.2 as i16 + amount).clamp(0, 255) as u8,
    )
}

fn is_too_dull(rgb: (u8, u8, u8)) -> bool {
    let max = rgb.0.max(rgb.1).max(rgb.2) as i16;
    let min = rgb.0.min(rgb.1).min(rgb.2) as i16;
    (max - min) < 18
}

/// Parse Omarchy's `colors.toml` (supports both state theme format and theme-dir format).
pub fn parse_omarchy_toml(content: &str) -> Option<Theme> {
    let mut map: HashMap<String, (u8, u8, u8)> = HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let key = k.trim().to_string();
            let val = v.trim();
            if let Some(rgb) = parse_hex_color(val) {
                map.insert(key, rgb);
            }
        }
    }

    if map.is_empty() {
        return None;
    }

    let bg = map
        .get("background")
        .or_else(|| map.get("color0"))
        .copied()
        .unwrap_or((16, 19, 21));
    let surface = map
        .get("lighter_background")
        .or_else(|| map.get("dark_background"))
        .copied()
        .unwrap_or_else(|| lighten(bg, 8));
    let fg = map
        .get("foreground")
        .or_else(|| map.get("color15"))
        .copied()
        .unwrap_or((230, 230, 230));
    let muted = map
        .get("muted")
        .or_else(|| map.get("dark_foreground"))
        .or_else(|| map.get("color8"))
        .copied()
        .unwrap_or((110, 118, 128));

    let raw_accent = map
        .get("accent")
        .or_else(|| map.get("color4"))
        .or_else(|| map.get("color6"))
        .copied()
        .unwrap_or((0, 229, 255));
    let accent = if is_too_dull(raw_accent) {
        map.get("bright_red")
            .or_else(|| map.get("bright_foreground"))
            .or_else(|| map.get("color3"))
            .copied()
            .unwrap_or(raw_accent)
    } else {
        raw_accent
    };
    let accent_secondary = map
        .get("bright_yellow")
        .or_else(|| map.get("magenta"))
        .or_else(|| map.get("color5"))
        .copied()
        .unwrap_or_else(|| lighten(accent, 24));
    let selection_bg = map
        .get("selection")
        .or_else(|| map.get("selection_background"))
        .copied()
        .unwrap_or_else(|| lighten(bg, 28));
    let selection_fg = map
        .get("selection_foreground")
        .or_else(|| map.get("bright_foreground"))
        .copied()
        .unwrap_or(fg);
    let border = map
        .get("muted")
        .or_else(|| map.get("color8"))
        .copied()
        .unwrap_or_else(|| lighten(bg, 35));
    let border_focused = map
        .get("active_border_color")
        .copied()
        .unwrap_or(accent);
    let success = map
        .get("green")
        .or_else(|| map.get("color2"))
        .copied()
        .unwrap_or((90, 214, 145));
    let warning = map
        .get("yellow")
        .or_else(|| map.get("color3"))
        .copied()
        .unwrap_or((242, 194, 92));
    let error = map
        .get("bright_red")
        .or_else(|| map.get("red"))
        .or_else(|| map.get("color1"))
        .copied()
        .unwrap_or((244, 96, 108));

    Some(Theme {
        bg: to_color(bg),
        surface: to_color(surface),
        fg: to_color(fg),
        muted: to_color(muted),
        accent: to_color(accent),
        accent_secondary: to_color(accent_secondary),
        selection_bg: to_color(selection_bg),
        selection_fg: to_color(selection_fg),
        border: to_color(border),
        border_focused: to_color(border_focused),
        success: to_color(success),
        warning: to_color(warning),
        error: to_color(error),
    })
}

pub fn load_omarchy_theme_from_path(path: &Path) -> Option<Theme> {
    let text = std::fs::read_to_string(path).ok()?;
    parse_omarchy_toml(&text)
}

pub fn load_omarchy_theme() -> Option<Theme> {
    load_omarchy_theme_from_path(&omarchy_theme_path())
}

pub fn load_pywal_theme() -> Option<Theme> {
    let path = dirs::home_dir()?.join(".cache").join("wal").join("colors.json");
    let text = std::fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let special = json.get("special");
    let colors = json.get("colors")?;

    let bg = special
        .and_then(|s| s.get("background")?.as_str())
        .and_then(parse_hex_color)
        .or_else(|| parse_hex_color(colors.get("color0")?.as_str()?))
        .unwrap_or((15, 17, 21));
    let fg = special
        .and_then(|s| s.get("foreground")?.as_str())
        .and_then(parse_hex_color)
        .or_else(|| parse_hex_color(colors.get("color7")?.as_str()?))
        .unwrap_or((234, 238, 244));
    let accent = parse_hex_color(colors.get("color4").or_else(|| colors.get("color1"))?.as_str()?)
        .unwrap_or((0, 229, 255));
    let accent_secondary = parse_hex_color(colors.get("color5").or_else(|| colors.get("color2"))?.as_str()?)
        .unwrap_or_else(|| lighten(accent, 25));
    let muted = parse_hex_color(colors.get("color8").and_then(|v| v.as_str()).unwrap_or("#666c77"))
        .unwrap_or((110, 118, 128));

    let mut theme = dark_theme();
    theme.bg = to_color(bg);
    theme.surface = to_color(lighten(bg, 8));
    theme.fg = to_color(fg);
    theme.muted = to_color(muted);
    theme.accent = to_color(accent);
    theme.accent_secondary = to_color(accent_secondary);
    theme.selection_bg = to_color(lighten(bg, 26));
    theme.selection_fg = to_color(fg);
    theme.border = to_color(muted);
    theme.border_focused = to_color(accent);
    Some(theme)
}

pub fn theme_from_album_palette(palette: &[(u8, u8, u8)]) -> Theme {
    let mut theme = dark_theme();
    if let Some(&primary) = palette.first() {
        theme.accent = to_color(primary);
        theme.border_focused = to_color(primary);
    }
    if let Some(&secondary) = palette.get(1) {
        theme.accent_secondary = to_color(secondary);
    } else {
        theme.accent_secondary = theme.accent;
    }
    theme
}

pub struct ThemeManager {
    pub mode: ThemeMode,
    pub omarchy_cache: Option<Theme>,
    pub pywal_cache: Option<Theme>,
    pub album_palette: Vec<(u8, u8, u8)>,
    pub beat_offset: usize,
    pub drop_active: bool,
}

impl ThemeManager {
    pub fn new(mode: ThemeMode) -> Self {
        Self {
            mode,
            omarchy_cache: load_omarchy_theme(),
            pywal_cache: load_pywal_theme(),
            album_palette: vec![(0, 229, 255), (166, 130, 255), (90, 214, 145)],
            beat_offset: 0,
            drop_active: false,
        }
    }

    pub fn reload_system_themes(&mut self) {
        self.omarchy_cache = load_omarchy_theme();
        self.pywal_cache = load_pywal_theme();
    }

    pub fn set_album_palette(&mut self, palette: Vec<(u8, u8, u8)>) {
        if !palette.is_empty() {
            self.album_palette = palette;
            self.beat_offset = 0;
        }
    }

    pub fn advance_beat(&mut self) {
        if !self.album_palette.is_empty() {
            self.beat_offset = (self.beat_offset + 1) % self.album_palette.len();
        }
    }

    pub fn active_theme(&self) -> Theme {
        match self.mode {
            ThemeMode::Omarchy => self.omarchy_cache.clone().unwrap_or_else(dark_theme),
            ThemeMode::Pywal => self.pywal_cache.clone().unwrap_or_else(dark_theme),
            ThemeMode::AlbumArt => theme_from_album_palette(&self.album_palette),
            ThemeMode::Dark => dark_theme(),
        }
    }

    pub fn visualizer_rgb(&self) -> (u8, u8, u8) {
        match self.mode {
            ThemeMode::AlbumArt => {
                if self.drop_active && !self.album_palette.is_empty() {
                    self.album_palette[self.beat_offset % self.album_palette.len()]
                } else {
                    self.album_palette.first().copied().unwrap_or((0, 229, 255))
                }
            }
            _ => match self.active_theme().accent {
                Color::Rgb(r, g, b) => (r, g, b),
                _ => (0, 229, 255),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_omarchy_state_toml_format() {
        let sample = r##"
mode = "dark"
accent = "#798186"
selection = "#343d41"
muted = "#4b4e55"
background = "#101315"
dark_background = "#0c0e10"
foreground = "#cacccc"
active_border_color = "#a8adb0"
green = "#9fa5a9"
bright_red = "#de6145"
"##;
        let t = parse_omarchy_toml(sample).expect("should parse omarchy toml");
        assert_eq!(t.bg, Color::Rgb(0x10, 0x13, 0x15));
        assert_eq!(t.fg, Color::Rgb(0xca, 0xcc, 0xcc));
        assert_eq!(t.selection_bg, Color::Rgb(0x34, 0x3d, 0x41));
        assert_eq!(t.error, Color::Rgb(0xde, 0x61, 0x45));
    }

    #[test]
    fn parses_live_system_omarchy_theme_if_present() {
        let path = omarchy_theme_path();
        if path.exists() {
            assert!(load_omarchy_theme_from_path(&path).is_some());
        }
    }
}
