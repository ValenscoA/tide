use crate::api::TrackLyrics;

#[derive(Debug, Clone, PartialEq)]
pub struct LyricLine {
    pub time: f64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LyricsData {
    Synced(Vec<LyricLine>),
    Plain(Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum LyricsState {
    Idle,
    Loading,
    Loaded(LyricsData),
    Unavailable,
}

pub fn parse(raw: &TrackLyrics) -> Option<LyricsData> {
    if let Some(ref subs) = raw.subtitles {
        let parsed = parse_lrc(subs);
        if !parsed.is_empty() {
            return Some(LyricsData::Synced(parsed));
        }
    }

    if let Some(ref plain) = raw.lyrics {
        let lines: Vec<String> = plain
            .lines()
            .map(|l| l.trim_end_matches('\r').trim().to_string())
            .collect();
        let first = lines.iter().position(|l| !l.is_empty());
        let last = lines.iter().rposition(|l| !l.is_empty());
        if let (Some(a), Some(b)) = (first, last) {
            return Some(LyricsData::Plain(lines[a..=b].to_vec()));
        }
    }

    None
}

pub fn parse_lrc(input: &str) -> Vec<LyricLine> {
    let mut offset_secs: f64 = 0.0;
    let mut out = Vec::new();

    for raw_line in input.lines() {
        let mut rest = raw_line.trim_end_matches('\r').trim();
        let mut timestamps = Vec::new();

        while let Some(after_open) = rest.strip_prefix('[') {
            let Some(close_idx) = after_open.find(']') else {
                break;
            };
            let tag = &after_open[..close_idx];
            if let Some(secs) = parse_timestamp(tag) {
                timestamps.push(secs);
                rest = after_open[close_idx + 1..].trim_start();
            } else {
                if let Some(ms_str) = tag.strip_prefix("offset:") {
                    if let Ok(ms) = ms_str.trim().parse::<f64>() {
                        offset_secs = ms / 1000.0;
                    }
                }
                rest = after_open[close_idx + 1..].trim_start();
            }
        }

        if timestamps.is_empty() {
            continue;
        }

        let text = rest.trim().to_string();
        for t in timestamps {
            out.push(LyricLine {
                time: (t - offset_secs).max(0.0),
                text: text.clone(),
            });
        }
    }

    out.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap_or(std::cmp::Ordering::Equal));
    out
}

fn parse_timestamp(tag: &str) -> Option<f64> {
    let (min_str, sec_str) = tag.split_once(':')?;
    if min_str.is_empty() || !min_str.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let mins: f64 = min_str.parse().ok()?;
    let secs: f64 = sec_str.parse().ok()?;
    if !(0.0..60.0).contains(&secs) {
        return None;
    }
    Some(mins * 60.0 + secs)
}

pub fn active_line_index(lines: &[LyricLine], elapsed: f64) -> Option<usize> {
    if lines.is_empty() || elapsed < lines[0].time {
        return None;
    }
    let idx = lines.partition_point(|l| l.time <= elapsed);
    Some(idx.saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_synced_lrc_with_offset() {
        let lrc = "[ti:Test]\n[offset:500]\n[00:01.50]Line 1\n[00:10.00][00:20.00]Chorus\n";
        let lines = parse_lrc(lrc);
        assert_eq!(lines.len(), 3);
        assert!((lines[0].time - 1.0).abs() < 1e-6);
        assert_eq!(lines[1].text, "Chorus");
    }

    #[test]
    fn active_line_tracks_elapsed() {
        let lines = vec![
            LyricLine { time: 2.0, text: "A".into() },
            LyricLine { time: 5.5, text: "B".into() },
            LyricLine { time: 9.0, text: "C".into() },
        ];
        assert_eq!(active_line_index(&lines, 1.0), None);
        assert_eq!(active_line_index(&lines, 4.0), Some(0));
        assert_eq!(active_line_index(&lines, 5.5), Some(1));
        assert_eq!(active_line_index(&lines, 15.0), Some(2));
    }
}
