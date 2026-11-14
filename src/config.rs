use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioQuality {
    Low,
    High,
    Lossless,
    HiResLossless,
}

impl Default for AudioQuality {
    fn default() -> Self {
        Self::Lossless
    }
}

impl AudioQuality {
    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "Low (AAC 96)",
            Self::High => "High (AAC 320)",
            Self::Lossless => "Lossless (FLAC)",
            Self::HiResLossless => "Hi-Res FLAC",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            Self::Low => "LOW",
            Self::High => "HIGH",
            Self::Lossless => "LOSSLESS",
            Self::HiResLossless => "HI-RES",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Low => Self::High,
            Self::High => Self::Lossless,
            Self::Lossless => Self::HiResLossless,
            Self::HiResLossless => Self::Low,
        }
    }

    pub fn fallback_chain(self) -> &'static [&'static str] {
        match self {
            Self::HiResLossless => &["HI_RES_LOSSLESS", "LOSSLESS", "HIGH", "LOW"],
            Self::Lossless => &["LOSSLESS", "HIGH", "LOW"],
            Self::High => &["HIGH", "LOW"],
            Self::Low => &["LOW"],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeMode {
    Omarchy,
    AlbumArt,
    Pywal,
    Dark,
}

impl ThemeMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Omarchy => "Omarchy",
            Self::AlbumArt => "Album Art",
            Self::Pywal => "Pywal",
            Self::Dark => "Classic Dark",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Omarchy => Self::AlbumArt,
            Self::AlbumArt => Self::Pywal,
            Self::Pywal => Self::Dark,
            Self::Dark => Self::Omarchy,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub audio_quality: AudioQuality,
    pub theme_mode: ThemeMode,
    pub volume: f32,
    pub output_device: Option<String>,
    pub download_dir: String,
    pub cache_enabled: bool,
    pub image_cover: bool,
    pub drop_detection: bool,
    pub calm_mode: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            audio_quality: AudioQuality::Lossless,
            theme_mode: ThemeMode::AlbumArt,
            volume: 0.5,
            output_device: None,
            download_dir: default_download_dir(),
            cache_enabled: true,
            image_cover: true,
            drop_detection: true,
            calm_mode: false,
        }
    }
}

impl Config {
    pub fn resolved_download_dir(&self) -> String {
        expand_tilde(&self.download_dir)
    }
}

pub fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).to_string_lossy().to_string();
        }
    } else if path == "~" {
        if let Some(home) = dirs::home_dir() {
            return home.to_string_lossy().to_string();
        }
    }
    path.to_string()
}

pub fn default_download_dir() -> String {
    dirs::audio_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Music")))
        .map(|p| p.join("Tidal").to_string_lossy().to_string())
        .unwrap_or_else(|| "./Downloads".to_string())
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config"))
        .join("tidal-tui")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".cache"))
        .join("tidal-tui")
        .join("audio")
}

pub fn omarchy_theme_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".local")
        .join("state")
        .join("omarchy")
        .join("current")
        .join("theme")
        .join("colors.toml")
}

pub fn load() -> Config {
    let path = config_path();
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(mut cfg) = serde_json::from_str::<Config>(&text) {
            cfg.volume = cfg.volume.clamp(0.0, 1.0);
            return cfg;
        }
    }
    Config::default()
}

pub fn save(cfg: &Config) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(cfg)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_fallback_chains_step_down() {
        assert_eq!(
            AudioQuality::HiResLossless.fallback_chain(),
            &["HI_RES_LOSSLESS", "LOSSLESS", "HIGH", "LOW"]
        );
        assert_eq!(
            AudioQuality::Lossless.fallback_chain(),
            &["LOSSLESS", "HIGH", "LOW"]
        );
        assert_eq!(AudioQuality::High.fallback_chain(), &["HIGH", "LOW"]);
        assert_eq!(AudioQuality::Low.fallback_chain(), &["LOW"]);
    }

    #[test]
    fn theme_mode_cycles_all_variants() {
        let start = ThemeMode::Omarchy;
        let next = start.next().next().next().next();
        assert_eq!(next, start);
    }

    #[test]
    fn config_deserializes_with_defaults() {
        let cfg: Config = serde_json::from_str(r#"{"volume": 0.75}"#).unwrap();
        assert!((cfg.volume - 0.75).abs() < 1e-6);
        assert_eq!(cfg.audio_quality, AudioQuality::Lossless);
        assert!(cfg.cache_enabled);
    }
}
