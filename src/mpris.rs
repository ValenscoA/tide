use std::sync::mpsc;
use std::time::Duration;

use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig};

pub struct MprisBridge {
    controls: Option<MediaControls>,
    rx: mpsc::Receiver<MediaControlEvent>,
    _cover_tmp: Option<tempfile::NamedTempFile>,
}

impl MprisBridge {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let controls = init_controls(tx);
        Self {
            controls,
            rx,
            _cover_tmp: None,
        }
    }

    pub fn poll_events(&self) -> Vec<MediaControlEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            out.push(ev);
        }
        out
    }

    pub fn update_metadata(
        &mut self,
        title: &str,
        artist: &str,
        album: &str,
        duration_secs: u64,
        cover_bytes: Option<&[u8]>,
    ) {
        let Some(ref mut mc) = self.controls else {
            return;
        };
        let mut cover_url = None;
        self._cover_tmp = cover_bytes.and_then(|bytes| {
            use std::io::Write;
            let mut tmp = tempfile::Builder::new().suffix(".jpg").tempfile().ok()?;
            tmp.write_all(bytes).ok()?;
            cover_url = Some(format!("file://{}", tmp.path().to_string_lossy()));
            Some(tmp)
        });
        let dur = if duration_secs > 0 {
            Some(Duration::from_secs(duration_secs))
        } else {
            None
        };
        let _ = mc.set_metadata(MediaMetadata {
            title: Some(title),
            artist: Some(artist),
            album: if album.is_empty() { None } else { Some(album) },
            cover_url: cover_url.as_deref(),
            duration: dur,
        });
    }

    pub fn update_playback(&mut self, playing: bool, paused: bool, position_ms: u64) {
        let Some(ref mut mc) = self.controls else {
            return;
        };
        let progress = Some(MediaPosition(Duration::from_millis(position_ms)));
        let state = if !playing && !paused {
            MediaPlayback::Stopped
        } else if paused {
            MediaPlayback::Paused { progress }
        } else {
            MediaPlayback::Playing { progress }
        };
        let _ = mc.set_playback(state);
    }
}

fn init_controls(tx: mpsc::Sender<MediaControlEvent>) -> Option<MediaControls> {
    #[cfg(target_os = "linux")]
    let dbus_name = format!("tide.instance{}", std::process::id());
    #[cfg(target_os = "linux")]
    let cfg = PlatformConfig {
        dbus_name: &dbus_name,
        display_name: "Tide",
        hwnd: None,
    };
    #[cfg(not(target_os = "linux"))]
    let cfg = PlatformConfig {
        dbus_name: "",
        display_name: "Tide",
        hwnd: None,
    };

    let mut controls = MediaControls::new(cfg).ok()?;
    let _ = controls.attach(move |ev| {
        let _ = tx.send(ev);
    });
    Some(controls)
}
