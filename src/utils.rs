use std::path::PathBuf;

/// Remove filesystem-unsafe characters and truncate to 200 chars.
pub fn safe_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if r#"\/*?:"<>|"#.contains(c) { '_' } else { c })
        .collect();
    let trimmed = cleaned.trim_matches(|c| c == '.' || c == ' ');
    trimmed.chars().take(200).collect()
}

/// Format seconds as `M:SS` or `H:MM:SS`.
pub fn fmt_time(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{}:{:02}:{:02}", h, m, s)
    } else {
        format!("{}:{:02}", m, s)
    }
}

/// Format byte sizes in a human-readable way.
pub fn fmt_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{} B", bytes)
    }
}

/// Detect audio file extension from the first 12 bytes of the file.
pub fn audio_extension(magic: &[u8]) -> &'static str {
    if magic.starts_with(b"fLaC") {
        "flac"
    } else if magic.len() >= 8 && &magic[4..8] == b"ftyp" {
        "m4a"
    } else if magic.starts_with(b"ID3") || (magic.len() >= 2 && magic[0] == 0xFF && (magic[1] & 0xE0) == 0xE0) {
        "mp3"
    } else {
        "flac"
    }
}

/// Check if a track is already saved in the output directory (.flac, .m4a, .mp3).
pub fn is_saved(out_dir: &str, artist: &str, title: &str) -> bool {
    let dir = PathBuf::from(out_dir);
    for ext in &["flac", "m4a", "mp3"] {
        let exact = safe_filename(&format!("{} - {}.{}", artist, title, ext));
        if dir.join(&exact).exists() {
            return true;
        }
    }
    let title_lower = safe_filename(title).to_lowercase();
    if title_lower.is_empty() {
        return false;
    }
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let fname = entry.file_name();
            let s = fname.to_string_lossy().to_lowercase();
            for ext in &[".flac", ".m4a", ".mp3"] {
                if s.ends_with(ext) {
                    let stem = &s[..s.len() - ext.len()];
                    if stem.contains(&title_lower) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_time_formats_seconds_and_hours() {
        assert_eq!(fmt_time(0.0), "0:00");
        assert_eq!(fmt_time(45.0), "0:45");
        assert_eq!(fmt_time(60.0), "1:00");
        assert_eq!(fmt_time(185.0), "3:05");
        assert_eq!(fmt_time(3665.0), "1:01:05");
    }

    #[test]
    fn fmt_bytes_scales_units() {
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(2048), "2 KB");
        assert_eq!(fmt_bytes(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn safe_filename_sanitizes_and_truncates() {
        assert_eq!(safe_filename("a/b\\c*d?e:f"), "a_b_c_d_e_f");
        assert_eq!(safe_filename("  .hello. "), "hello");
        assert_eq!(safe_filename(&"a".repeat(300)).len(), 200);
    }

    #[test]
    fn audio_extension_detects_formats() {
        assert_eq!(audio_extension(b"fLaC\x00\x00\x00\x22"), "flac");
        assert_eq!(audio_extension(b"\x00\x00\x00\x1cftypisom"), "m4a");
        assert_eq!(audio_extension(b"ID3\x04\x00\x00"), "mp3");
    }

    #[test]
    fn is_saved_detects_existing_tracks() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().to_string_lossy().to_string();
        assert!(!is_saved(&out, "Artist", "Track"));
        std::fs::File::create(dir.path().join(safe_filename("Artist - Track.m4a"))).unwrap();
        assert!(is_saved(&out, "Artist", "Track"));
    }
}
