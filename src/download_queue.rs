use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::api::{TidalClient, TrackInfo};
use crate::auth::TIDAL_UA;
use crate::config::AudioQuality;
use crate::utils::{audio_extension, is_saved, safe_filename};

struct QueueEntry {
    track: TrackInfo,
    client: TidalClient,
    quality: AudioQuality,
    output_dir: String,
}

pub struct DownloadQueue {
    pending: Arc<Mutex<VecDeque<QueueEntry>>>,
    pub done: Arc<AtomicU64>,
    pub total: Arc<AtomicU64>,
    pub current: Arc<Mutex<Option<String>>>,
    pub last_error: Arc<Mutex<Option<String>>>,
}

impl DownloadQueue {
    pub fn new() -> Self {
        let pending: Arc<Mutex<VecDeque<QueueEntry>>> = Arc::new(Mutex::new(VecDeque::new()));
        let done = Arc::new(AtomicU64::new(0));
        let total = Arc::new(AtomicU64::new(0));
        let current: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let last_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        let pending_w = pending.clone();
        let done_w = done.clone();
        let current_w = current.clone();
        let err_w = last_error.clone();

        thread::spawn(move || {
            let http = reqwest::blocking::Client::builder()
                .user_agent(TIDAL_UA)
                .timeout(std::time::Duration::from_secs(90))
                .build()
                .unwrap_or_default();

            loop {
                let entry = {
                    let mut q = pending_w.lock().unwrap_or_else(|e| e.into_inner());
                    q.pop_front()
                };
                match entry {
                    None => thread::sleep(Duration::from_millis(250)),
                    Some(entry) => {
                        let label = format!("{} - {}", entry.track.artist_name, entry.track.title);
                        *current_w.lock().unwrap_or_else(|e| e.into_inner()) = Some(label);
                        if let Err(e) = download_track(&entry, &http) {
                            *err_w.lock().unwrap_or_else(|e| e.into_inner()) = Some(e.to_string());
                        }
                        *current_w.lock().unwrap_or_else(|e| e.into_inner()) = None;
                        done_w.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        });

        Self {
            pending,
            done,
            total,
            current,
            last_error,
        }
    }

    pub fn push_tracks(
        &self,
        tracks: Vec<TrackInfo>,
        client: TidalClient,
        quality: AudioQuality,
        output_dir: String,
    ) -> usize {
        let mut q = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let mut added = 0usize;
        for track in tracks {
            if track.id == 0 {
                continue;
            }
            if !is_saved(&output_dir, &track.artist_name, &track.title) {
                q.push_back(QueueEntry {
                    track,
                    client: client.clone(),
                    quality,
                    output_dir: output_dir.clone(),
                });
                added += 1;
            }
        }
        drop(q);
        if added > 0 {
            self.total.fetch_add(added as u64, Ordering::Relaxed);
        }
        added
    }

    pub fn status(&self) -> Option<String> {
        let current = self.current.lock().unwrap_or_else(|e| e.into_inner()).clone();
        current.map(|name| {
            let done = self.done.load(Ordering::Relaxed);
            let total = self.total.load(Ordering::Relaxed);
            format!("⬇ {} ({}/{})", name, done + 1, total)
        })
    }

    pub fn take_last_error(&self) -> Option<String> {
        self.last_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }
}

fn download_track(entry: &QueueEntry, http: &reqwest::blocking::Client) -> anyhow::Result<()> {
    let out_dir = std::path::Path::new(&entry.output_dir);
    std::fs::create_dir_all(out_dir)?;
    if is_saved(&entry.output_dir, &entry.track.artist_name, &entry.track.title) {
        return Ok(());
    }

    let stream = entry.client.resolve_stream(entry.track.id, entry.quality)?;
    let tmp_name = safe_filename(&format!(
        "{} - {}.tmp",
        entry.track.artist_name, entry.track.title
    ));
    let tmp_path = out_dir.join(&tmp_name);
    let mut f = std::fs::File::create(&tmp_path)?;

    use ctr::cipher::{KeyIvInit, StreamCipher};
    type Aes128Ctr = ctr::Ctr64BE<aes::Aes128>;
    let mut maybe_cipher = stream.encryption.map(|enc| {
        let mut iv = [0u8; 16];
        iv[..8].copy_from_slice(&enc.nonce);
        Aes128Ctr::new_from_slices(&enc.key, &iv).expect("valid key/iv")
    });

    let mut buf = vec![0u8; 65_536];
    for url in &stream.urls {
        let mut reader = http.get(url).send()?.error_for_status()?;
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            let chunk = &mut buf[..n];
            if let Some(ref mut cipher) = maybe_cipher {
                cipher.apply_keystream(chunk);
            }
            f.write_all(chunk)?;
        }
    }
    drop(f);

    let ext = {
        let mut magic = [0u8; 12];
        let n = std::fs::File::open(&tmp_path)
            .and_then(|mut fh| fh.read(&mut magic))
            .unwrap_or(0);
        audio_extension(&magic[..n])
    };
    let filename = safe_filename(&format!(
        "{} - {}.{}",
        entry.track.artist_name, entry.track.title, ext
    ));
    let dest = out_dir.join(&filename);
    std::fs::rename(&tmp_path, &dest)?;

    let cover = entry
        .track
        .album_cover
        .as_deref()
        .and_then(|id| entry.client.fetch_cover(id, 1280).ok());
    let _ = crate::metadata::embed(&dest, &entry.track, cover.as_deref());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Session;

    fn dummy_session() -> Session {
        Session {
            access_token: "a".into(),
            refresh_token: "b".into(),
            expiry_time: "2099-01-01T00:00:00Z".into(),
            user_id: 1,
            country_code: "US".into(),
            user_name: None,
            session_id: None,
            token_type: "Bearer".into(),
        }
    }

    fn dummy_track(id: u64, title: &str) -> TrackInfo {
        TrackInfo {
            id,
            title: title.into(),
            artist_name: "Artist".into(),
            artist_id: None,
            artists: vec!["Artist".into()],
            album_name: "Album".into(),
            album_id: None,
            album_cover: None,
            album_artist: "Artist".into(),
            album_copyright: None,
            album_release_year: None,
            track_num: 1,
            volume_num: 1,
            isrc: None,
            audio_quality: "LOSSLESS".into(),
            duration: 180,
            explicit: false,
            hires: false,
            local_path: None,
        }
    }

    #[test]
    fn skips_already_saved_and_counts_new_tracks() {
        let q = DownloadQueue::new();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().to_string_lossy().to_string();
        std::fs::File::create(dir.path().join(safe_filename("Artist - Saved.flac"))).unwrap();
        let client = TidalClient::new(dummy_session());
        let added = q.push_tracks(
            vec![dummy_track(1, "Saved"), dummy_track(2, "Fresh")],
            client,
            AudioQuality::Lossless,
            out,
        );
        assert_eq!(added, 1);
        assert_eq!(q.total.load(Ordering::Relaxed), 1);
    }
}
