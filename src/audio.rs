use std::collections::VecDeque;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::{FormatOptions, SeekMode, SeekTo};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;

use crate::api::{EncryptionInfo, TidalClient, TrackInfo};
use crate::auth::TIDAL_UA;
use crate::config::{self, AudioQuality};
use crate::spectrum::{self, FFT_SIZE};

const BUFFER_THRESHOLD: u64 = 65_536;
const TARGET_ANALYSIS_SR: u32 = 22_050;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepeatMode {
    Off,
    All,
    One,
}

impl RepeatMode {
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::All => "All",
            Self::One => "One",
        }
    }
}

pub enum AudioCommand {
    PlayTidal {
        generation: u64,
        track: TrackInfo,
        client: TidalClient,
        quality: AudioQuality,
        cache_enabled: bool,
        analyze_drops: bool,
    },
    PlayLocal {
        generation: u64,
        path: PathBuf,
        track: TrackInfo,
        analyze_drops: bool,
    },
    TogglePause,
    SetPaused(bool),
    Stop,
    SeekToMs(u64),
    SeekRelativeMs(i64),
    SetVolume(f32),
    SetOutputDevice(Option<String>),
}

pub struct AudioTelemetry {
    pub generation: AtomicU64,
    pub is_playing: AtomicBool,
    pub is_paused: AtomicBool,
    pub is_buffering: AtomicBool,
    pub track_ended_generation: AtomicU64,
    pub position_ms: AtomicU64,
    pub duration_ms: AtomicU64,
    pub sample_rate: AtomicU32,
    pub downloaded_bytes: AtomicU64,
    pub total_bytes: AtomicU64,
    pub active_quality: Mutex<String>,
    pub active_codec: Mutex<String>,
    pub error: Mutex<Option<String>>,
    pub spec_buf: Mutex<Vec<f32>>,
    pub beats: Mutex<Vec<f64>>,
    pub drops: Mutex<Vec<f64>>,
}

impl AudioTelemetry {
    fn new() -> Self {
        Self {
            generation: AtomicU64::new(0),
            is_playing: AtomicBool::new(false),
            is_paused: AtomicBool::new(false),
            is_buffering: AtomicBool::new(false),
            track_ended_generation: AtomicU64::new(0),
            position_ms: AtomicU64::new(0),
            duration_ms: AtomicU64::new(0),
            sample_rate: AtomicU32::new(44100),
            downloaded_bytes: AtomicU64::new(0),
            total_bytes: AtomicU64::new(0),
            active_quality: Mutex::new(String::new()),
            active_codec: Mutex::new(String::new()),
            error: Mutex::new(None),
            spec_buf: Mutex::new(vec![0.0f32; FFT_SIZE]),
            beats: Mutex::new(Vec::new()),
            drops: Mutex::new(Vec::new()),
        }
    }
}

pub struct AudioEngine {
    tx: mpsc::Sender<AudioCommand>,
    pub telemetry: Arc<AudioTelemetry>,
    next_gen: AtomicU64,
}

impl AudioEngine {
    pub fn spawn(initial_volume: f32, initial_device: Option<String>) -> Self {
        let (tx, rx) = mpsc::channel::<AudioCommand>();
        let telemetry = Arc::new(AudioTelemetry::new());
        let tel_clone = telemetry.clone();

        thread::spawn(move || {
            supervisor_loop(rx, tel_clone, initial_volume, initial_device);
        });

        Self {
            tx,
            telemetry,
            next_gen: AtomicU64::new(1),
        }
    }

    pub fn play_tidal(
        &self,
        track: TrackInfo,
        client: TidalClient,
        quality: AudioQuality,
        cache_enabled: bool,
        analyze_drops: bool,
    ) -> u64 {
        let generation = self.next_gen.fetch_add(1, Ordering::Relaxed);
        let _ = self.tx.send(AudioCommand::PlayTidal {
            generation,
            track,
            client,
            quality,
            cache_enabled,
            analyze_drops,
        });
        generation
    }

    pub fn play_local(&self, path: PathBuf, track: TrackInfo, analyze_drops: bool) -> u64 {
        let generation = self.next_gen.fetch_add(1, Ordering::Relaxed);
        let _ = self.tx.send(AudioCommand::PlayLocal {
            generation,
            path,
            track,
            analyze_drops,
        });
        generation
    }

    pub fn toggle_pause(&self) {
        let _ = self.tx.send(AudioCommand::TogglePause);
    }

    pub fn set_paused(&self, paused: bool) {
        let _ = self.tx.send(AudioCommand::SetPaused(paused));
    }

    pub fn stop(&self) {
        let _ = self.tx.send(AudioCommand::Stop);
    }

    pub fn seek_to_ms(&self, ms: u64) {
        let _ = self.tx.send(AudioCommand::SeekToMs(ms));
    }

    pub fn seek_relative_ms(&self, delta_ms: i64) {
        let _ = self.tx.send(AudioCommand::SeekRelativeMs(delta_ms));
    }

    pub fn set_volume(&self, vol: f32) {
        let _ = self.tx.send(AudioCommand::SetVolume(vol.clamp(0.0, 1.0)));
    }

    pub fn set_output_device(&self, name: Option<String>) {
        let _ = self.tx.send(AudioCommand::SetOutputDevice(name));
    }
}

pub fn list_output_devices() -> Vec<String> {
    let host = cpal::default_host();
    let Ok(devs) = host.output_devices() else {
        return Vec::new();
    };
    let mut names: Vec<String> = devs.filter_map(|d| d.name().ok()).collect();
    names.sort();
    names.dedup();
    names
}

pub fn cache_stats() -> (u64, usize) {
    let dir = config::cache_dir();
    let Ok(entries) = fs::read_dir(dir) else {
        return (0, 0);
    };
    let mut bytes = 0u64;
    let mut count = 0usize;
    for entry in entries.flatten() {
        if let Ok(meta) = entry.metadata() {
            if meta.is_file() {
                bytes += meta.len();
                count += 1;
            }
        }
    }
    (bytes, count)
}

pub fn clear_cache() -> Result<()> {
    let dir = config::cache_dir();
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    fs::create_dir_all(&dir)?;
    Ok(())
}

struct ActivePlayback {
    generation: u64,
    stop: Arc<AtomicBool>,
    seek_req_ms: Arc<Mutex<Option<u64>>>,
    _stream: Option<cpal::Stream>,
}

fn supervisor_loop(
    rx: mpsc::Receiver<AudioCommand>,
    telemetry: Arc<AudioTelemetry>,
    initial_volume: f32,
    initial_device: Option<String>,
) {
    let volume = Arc::new(Mutex::new(initial_volume.clamp(0.0, 1.0)));
    let mut output_device = initial_device;
    let mut active: Option<ActivePlayback> = None;

    let (ready_tx, ready_rx) = mpsc::channel::<PreparedAudio>();

    loop {
        // Drain control commands
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                AudioCommand::PlayTidal {
                    generation,
                    track,
                    client,
                    quality,
                    cache_enabled,
                    analyze_drops,
                } => {
                    stop_active(&mut active, &telemetry);
                    telemetry.generation.store(generation, Ordering::Relaxed);
                    telemetry.is_buffering.store(true, Ordering::Relaxed);
                    telemetry.is_playing.store(false, Ordering::Relaxed);
                    telemetry.is_paused.store(false, Ordering::Relaxed);
                    telemetry.position_ms.store(0, Ordering::Relaxed);
                    telemetry.duration_ms.store(track.duration * 1000, Ordering::Relaxed);
                    telemetry.downloaded_bytes.store(0, Ordering::Relaxed);
                    telemetry.total_bytes.store(0, Ordering::Relaxed);
                    *telemetry.error.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    *telemetry.beats.lock().unwrap_or_else(|e| e.into_inner()) = Vec::new();
                    *telemetry.drops.lock().unwrap_or_else(|e| e.into_inner()) = Vec::new();

                    let stop = Arc::new(AtomicBool::new(false));
                    let seek_req = Arc::new(Mutex::new(None));
                    active = Some(ActivePlayback {
                        generation,
                        stop: stop.clone(),
                        seek_req_ms: seek_req.clone(),
                        _stream: None,
                    });

                    let ready_tx_c = ready_tx.clone();
                    let tel_c = telemetry.clone();
                    thread::spawn(move || {
                        prepare_tidal_playback(
                            generation,
                            track,
                            client,
                            quality,
                            cache_enabled,
                            analyze_drops,
                            stop,
                            seek_req,
                            tel_c,
                            ready_tx_c,
                        );
                    });
                }
                AudioCommand::PlayLocal {
                    generation,
                    path,
                    track,
                    analyze_drops,
                } => {
                    stop_active(&mut active, &telemetry);
                    telemetry.generation.store(generation, Ordering::Relaxed);
                    telemetry.is_buffering.store(true, Ordering::Relaxed);
                    telemetry.is_playing.store(false, Ordering::Relaxed);
                    telemetry.is_paused.store(false, Ordering::Relaxed);
                    telemetry.position_ms.store(0, Ordering::Relaxed);
                    telemetry.duration_ms.store(track.duration * 1000, Ordering::Relaxed);
                    *telemetry.error.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    *telemetry.active_quality.lock().unwrap_or_else(|e| e.into_inner()) =
                        track.audio_quality.clone();
                    *telemetry.active_codec.lock().unwrap_or_else(|e| e.into_inner()) = "LOCAL".into();
                    *telemetry.beats.lock().unwrap_or_else(|e| e.into_inner()) = Vec::new();
                    *telemetry.drops.lock().unwrap_or_else(|e| e.into_inner()) = Vec::new();

                    let stop = Arc::new(AtomicBool::new(false));
                    let seek_req = Arc::new(Mutex::new(None));
                    active = Some(ActivePlayback {
                        generation,
                        stop: stop.clone(),
                        seek_req_ms: seek_req.clone(),
                        _stream: None,
                    });

                    let ready_tx_c = ready_tx.clone();
                    let tel_c = telemetry.clone();
                    thread::spawn(move || {
                        let done = Arc::new(AtomicBool::new(true));
                        let _ = ready_tx_c.send(PreparedAudio {
                            generation,
                            path,
                            ext_hint: None,
                            download_done: done,
                            stop,
                            seek_req_ms: seek_req,
                            cleanup_on_stop: false,
                            analyze_drops,
                            fallback_duration_ms: track.duration * 1000,
                            telemetry: tel_c,
                        });
                    });
                }
                AudioCommand::TogglePause => {
                    let cur = telemetry.is_paused.load(Ordering::Relaxed);
                    telemetry.is_paused.store(!cur, Ordering::Relaxed);
                }
                AudioCommand::SetPaused(p) => {
                    telemetry.is_paused.store(p, Ordering::Relaxed);
                }
                AudioCommand::Stop => {
                    stop_active(&mut active, &telemetry);
                    telemetry.is_playing.store(false, Ordering::Relaxed);
                    telemetry.is_buffering.store(false, Ordering::Relaxed);
                    telemetry.is_paused.store(false, Ordering::Relaxed);
                    telemetry.position_ms.store(0, Ordering::Relaxed);
                }
                AudioCommand::SeekToMs(ms) => {
                    if let Some(ref act) = active {
                        let dur = telemetry.duration_ms.load(Ordering::Relaxed);
                        let clamped = if dur > 0 { ms.min(dur.saturating_sub(250)) } else { ms };
                        *act.seek_req_ms.lock().unwrap_or_else(|e| e.into_inner()) = Some(clamped);
                        telemetry.position_ms.store(clamped, Ordering::Relaxed);
                    }
                }
                AudioCommand::SeekRelativeMs(delta) => {
                    if let Some(ref act) = active {
                        let cur = telemetry.position_ms.load(Ordering::Relaxed) as i64;
                        let dur = telemetry.duration_ms.load(Ordering::Relaxed);
                        let target = (cur + delta).max(0) as u64;
                        let clamped = if dur > 0 {
                            target.min(dur.saturating_sub(250))
                        } else {
                            target
                        };
                        *act.seek_req_ms.lock().unwrap_or_else(|e| e.into_inner()) = Some(clamped);
                        telemetry.position_ms.store(clamped, Ordering::Relaxed);
                    }
                }
                AudioCommand::SetVolume(v) => {
                    *volume.lock().unwrap_or_else(|e| e.into_inner()) = v.clamp(0.0, 1.0);
                }
                AudioCommand::SetOutputDevice(dev) => {
                    output_device = dev;
                }
            }
        }

        // Check for newly prepared audio ready to open cpal stream
        while let Ok(prep) = ready_rx.try_recv() {
            let current_gen = telemetry.generation.load(Ordering::Relaxed);
            if prep.generation != current_gen || prep.stop.load(Ordering::Relaxed) {
                continue;
            }
            match start_audio_pipeline(prep, &output_device, &volume) {
                Ok(stream) => {
                    if let Some(ref mut act) = active {
                        if act.generation == current_gen {
                            act._stream = Some(stream);
                            telemetry.is_buffering.store(false, Ordering::Relaxed);
                            telemetry.is_playing.store(true, Ordering::Relaxed);
                        }
                    }
                }
                Err(e) => {
                    telemetry.is_buffering.store(false, Ordering::Relaxed);
                    telemetry.is_playing.store(false, Ordering::Relaxed);
                    *telemetry.error.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(format!("Audio playback failed: {}", e));
                }
            }
        }

        thread::sleep(Duration::from_millis(20));
    }
}

fn stop_active(active: &mut Option<ActivePlayback>, telemetry: &AudioTelemetry) {
    if let Some(mut act) = active.take() {
        act.stop.store(true, Ordering::Relaxed);
        act._stream.take();
    }
    if let Ok(mut sb) = telemetry.spec_buf.lock() {
        sb.fill(0.0);
    }
}

struct PreparedAudio {
    generation: u64,
    path: PathBuf,
    ext_hint: Option<String>,
    download_done: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    seek_req_ms: Arc<Mutex<Option<u64>>>,
    cleanup_on_stop: bool,
    analyze_drops: bool,
    fallback_duration_ms: u64,
    telemetry: Arc<AudioTelemetry>,
}

fn prepare_tidal_playback(
    generation: u64,
    track: TrackInfo,
    client: TidalClient,
    quality: AudioQuality,
    cache_enabled: bool,
    analyze_drops: bool,
    stop: Arc<AtomicBool>,
    seek_req_ms: Arc<Mutex<Option<u64>>>,
    telemetry: Arc<AudioTelemetry>,
    ready_tx: mpsc::Sender<PreparedAudio>,
) {
    if let Some(ref local) = track.local_path {
        let p = PathBuf::from(local);
        if p.exists() {
            let _ = ready_tx.send(PreparedAudio {
                generation,
                path: p,
                ext_hint: None,
                download_done: Arc::new(AtomicBool::new(true)),
                stop,
                seek_req_ms,
                cleanup_on_stop: false,
                analyze_drops,
                fallback_duration_ms: track.duration * 1000,
                telemetry,
            });
            return;
        }
    }

    let stream = match client.resolve_stream(track.id, quality) {
        Ok(s) => s,
        Err(e) => {
            if !stop.load(Ordering::Relaxed) {
                telemetry.is_buffering.store(false, Ordering::Relaxed);
                *telemetry.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(e.to_string());
            }
            return;
        }
    };

    if stop.load(Ordering::Relaxed) {
        return;
    }

    *telemetry.active_quality.lock().unwrap_or_else(|e| e.into_inner()) =
        stream.actual_quality.clone();
    *telemetry.active_codec.lock().unwrap_or_else(|e| e.into_inner()) = if stream.codecs.is_empty() {
        stream.extension.to_uppercase()
    } else {
        stream.codecs.clone()
    };

    let ext = if stream.extension.is_empty() {
        "m4a".to_string()
    } else {
        stream.extension.clone()
    };

    let cache_dir = config::cache_dir();
    let final_cache_path = cache_dir.join(format!("{}-{}.{}", track.id, stream.actual_quality, ext));

    if cache_enabled {
        if let Ok(meta) = fs::metadata(&final_cache_path) {
            if meta.len() > 4096 {
                telemetry.downloaded_bytes.store(meta.len(), Ordering::Relaxed);
                telemetry.total_bytes.store(meta.len(), Ordering::Relaxed);
                let _ = ready_tx.send(PreparedAudio {
                    generation,
                    path: final_cache_path,
                    ext_hint: Some(ext),
                    download_done: Arc::new(AtomicBool::new(true)),
                    stop,
                    seek_req_ms,
                    cleanup_on_stop: false,
                    analyze_drops,
                    fallback_duration_ms: track.duration * 1000,
                    telemetry,
                });
                return;
            }
        }
    }

    let (work_path, part_path, cleanup_on_stop) = if cache_enabled {
        let _ = fs::create_dir_all(&cache_dir);
        let part = cache_dir.join(format!(
            "{}-{}.{}.{}.part",
            track.id, stream.actual_quality, ext, generation
        ));
        (final_cache_path.clone(), part, false)
    } else {
        let part = std::env::temp_dir().join(format!("tidal-tui-{}-{}.{}", track.id, generation, ext));
        (part.clone(), part, true)
    };

    let download_done = Arc::new(AtomicBool::new(false));
    let header_ready = Arc::new(AtomicBool::new(false));

    let urls = stream.urls;
    let enc = stream.encryption;
    let part_dl = part_path.clone();
    let final_dl = work_path.clone();
    let dd_dl = download_done.clone();
    let hr_dl = header_ready.clone();
    let stop_dl = stop.clone();
    let tel_dl = telemetry.clone();

    thread::spawn(move || {
        let ok = stream_download_to_file(
            &urls,
            &part_dl,
            enc,
            &stop_dl,
            &hr_dl,
            &tel_dl,
        );
        if ok && cache_enabled && !stop_dl.load(Ordering::Relaxed) && part_dl != final_dl {
            let _ = fs::rename(&part_dl, &final_dl);
        }
        hr_dl.store(true, Ordering::Relaxed);
        dd_dl.store(true, Ordering::Relaxed);
    });

    while !header_ready.load(Ordering::Relaxed) {
        if stop.load(Ordering::Relaxed) {
            let _ = fs::remove_file(&part_path);
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }

    if stop.load(Ordering::Relaxed) {
        let _ = fs::remove_file(&part_path);
        return;
    }

    let read_path = if cache_enabled && final_cache_path.exists() && !part_path.exists() {
        final_cache_path
    } else {
        part_path
    };

    let _ = ready_tx.send(PreparedAudio {
        generation,
        path: read_path,
        ext_hint: Some(ext),
        download_done,
        stop,
        seek_req_ms,
        cleanup_on_stop,
        analyze_drops,
        fallback_duration_ms: track.duration * 1000,
        telemetry,
    });
}

fn stream_download_to_file(
    urls: &[String],
    dest: &Path,
    encryption: Option<EncryptionInfo>,
    stop: &AtomicBool,
    header_ready: &AtomicBool,
    telemetry: &AudioTelemetry,
) -> bool {
    use ctr::cipher::{KeyIvInit, StreamCipher};
    type Aes128Ctr = ctr::Ctr64BE<aes::Aes128>;

    let mut maybe_cipher = encryption.map(|enc| {
        let mut iv = [0u8; 16];
        iv[..8].copy_from_slice(&enc.nonce);
        Aes128Ctr::new_from_slices(&enc.key, &iv).expect("valid key/iv")
    });

    let http = reqwest::blocking::Client::builder()
        .user_agent(TIDAL_UA)
        .timeout(Duration::from_secs(60))
        .build()
        .unwrap_or_default();

    let mut file = match fs::File::create(dest) {
        Ok(f) => f,
        Err(e) => {
            *telemetry.error.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(format!("Cache write failed: {}", e));
            return false;
        }
    };

    let mut written: u64 = 0;
    let mut buf = vec![0u8; 65_536];

    for (idx, url) in urls.iter().enumerate() {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let resp = match http.get(url).send().and_then(|r| r.error_for_status()) {
            Ok(r) => r,
            Err(e) => {
                *telemetry.error.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(format!("Stream download error: {}", e));
                return false;
            }
        };
        if urls.len() == 1 && idx == 0 {
            if let Some(len) = resp.content_length() {
                telemetry.total_bytes.store(len, Ordering::Relaxed);
            }
        }
        let mut reader = resp;
        loop {
            if stop.load(Ordering::Relaxed) {
                return false;
            }
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = &mut buf[..n];
                    if let Some(ref mut cipher) = maybe_cipher {
                        cipher.apply_keystream(chunk);
                    }
                    if file.write_all(chunk).is_err() {
                        return false;
                    }
                    written += n as u64;
                    telemetry.downloaded_bytes.store(written, Ordering::Relaxed);
                    if !header_ready.load(Ordering::Relaxed) && written >= BUFFER_THRESHOLD {
                        header_ready.store(true, Ordering::Relaxed);
                    }
                }
                Err(_) => return false,
            }
        }
    }
    let _ = file.flush();
    true
}

struct StreamingFile {
    file: fs::File,
    path: PathBuf,
    download_done: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl io::Read for StreamingFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.file.read(buf) {
                Ok(0) => {
                    if self.stop.load(Ordering::Relaxed) || self.download_done.load(Ordering::Relaxed) {
                        return Ok(0);
                    }
                    thread::sleep(Duration::from_millis(15));
                }
                other => return other,
            }
        }
    }
}

impl Seek for StreamingFile {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.file.seek(pos)
    }
}

impl MediaSource for StreamingFile {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        fs::metadata(&self.path).ok().map(|m| m.len())
    }
}

fn open_probed_format(
    path: &Path,
    ext_hint: Option<&str>,
    download_done: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
) -> Result<(Box<dyn symphonia::core::formats::FormatReader>, u32, usize, u64)> {
    let file = fs::File::open(path)?;
    let streaming = StreamingFile {
        file,
        path: path.to_path_buf(),
        download_done,
        stop,
    };
    let mss = MediaSourceStream::new(Box::new(streaming), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = ext_hint {
        hint.with_extension(ext);
    } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions {
            enable_gapless: true,
            ..Default::default()
        },
        &MetadataOptions::default(),
    )?;
    let track = probed
        .format
        .default_track()
        .ok_or_else(|| anyhow!("No audio track found"))?;
    let sr = track.codec_params.sample_rate.unwrap_or(44100);
    let ch = track.codec_params.channels.map(|c| c.count()).unwrap_or(2);
    let n_frames = track.codec_params.n_frames.unwrap_or(0);
    Ok((probed.format, sr, ch, n_frames))
}

fn start_audio_pipeline(
    prep: PreparedAudio,
    output_device_name: &Option<String>,
    volume: &Arc<Mutex<f32>>,
) -> Result<cpal::Stream> {
    let (mut format, file_sr, file_ch, n_frames) = open_probed_format(
        &prep.path,
        prep.ext_hint.as_deref(),
        prep.download_done.clone(),
        prep.stop.clone(),
    )?;

    let duration_ms = if n_frames > 0 && file_sr > 0 {
        (n_frames * 1000) / file_sr as u64
    } else {
        prep.fallback_duration_ms
    };
    prep.telemetry.duration_ms.store(duration_ms, Ordering::Relaxed);
    prep.telemetry.sample_rate.store(file_sr, Ordering::Relaxed);

    let host = cpal::default_host();
    let device = if let Some(ref target) = output_device_name {
        host.output_devices()
            .ok()
            .and_then(|mut devs| devs.find(|d| d.name().ok().as_deref() == Some(target.as_str())))
            .or_else(|| host.default_output_device())
    } else {
        host.default_output_device()
    }
    .ok_or_else(|| anyhow!("No audio output device available"))?;

    let default_cfg = device.default_output_config()?;
    let device_sr = default_cfg.sample_rate().0;
    let device_ch = default_cfg.channels() as usize;

    let cpal_cfg = cpal::StreamConfig {
        channels: device_ch as cpal::ChannelCount,
        sample_rate: cpal::SampleRate(device_sr),
        buffer_size: cpal::BufferSize::Default,
    };

    let track_info = format
        .default_track()
        .ok_or_else(|| anyhow!("No default track"))?
        .clone();
    let track_id = track_info.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track_info.codec_params, &DecoderOptions::default())?;

    let audio_buf: Arc<Mutex<VecDeque<f32>>> =
        Arc::new(Mutex::new(VecDeque::with_capacity(device_sr as usize * device_ch * 2)));
    let decode_finished = Arc::new(AtomicBool::new(false));
    let seek_base_ms = Arc::new(AtomicU64::new(0));
    let consumed_device_frames = Arc::new(AtomicU64::new(0));

    // Background beat/drop analysis once downloaded
    if prep.analyze_drops {
        let p_ana = prep.path.clone();
        let dd_ana = prep.download_done.clone();
        let stop_ana = prep.stop.clone();
        let tel_ana = prep.telemetry.clone();
        let gen_ana = prep.generation;
        thread::spawn(move || {
            while !dd_ana.load(Ordering::Relaxed) {
                if stop_ana.load(Ordering::Relaxed) {
                    return;
                }
                thread::sleep(Duration::from_millis(200));
            }
            if stop_ana.load(Ordering::Relaxed) {
                return;
            }
            if let Ok(mono) = load_mono_f32(&p_ana, TARGET_ANALYSIS_SR, &stop_ana) {
                if tel_ana.generation.load(Ordering::Relaxed) == gen_ana {
                    let beats = spectrum::detect_beats(&mono, TARGET_ANALYSIS_SR);
                    let drops = spectrum::detect_drops(&mono, TARGET_ANALYSIS_SR);
                    *tel_ana.beats.lock().unwrap_or_else(|e| e.into_inner()) = beats;
                    *tel_ana.drops.lock().unwrap_or_else(|e| e.into_inner()) = drops;
                }
            }
        });
    }

    // Decode thread
    {
        let audio_buf_d = audio_buf.clone();
        let decode_fin_d = decode_finished.clone();
        let seek_base_d = seek_base_ms.clone();
        let consumed_d = consumed_device_frames.clone();
        let stop_d = prep.stop.clone();
        let seek_req_d = prep.seek_req_ms.clone();
        let tel_d = prep.telemetry.clone();
        let path_d = prep.path.clone();
        let cleanup_d = prep.cleanup_on_stop;
        let gen_d = prep.generation;

        thread::spawn(move || {
            let max_buf_samples = (device_sr as usize * device_ch) / 2; // ~0.5s buffer for snappy seek/sync
            loop {
                if stop_d.load(Ordering::Relaxed) {
                    break;
                }

                // Handle pending seek request
                if let Some(target_ms) = seek_req_d.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    let secs = target_ms / 1000;
                    let frac = (target_ms % 1000) as f64 / 1000.0;
                    if format
                        .seek(
                            SeekMode::Coarse,
                            SeekTo::Time {
                                time: Time::new(secs, frac),
                                track_id: Some(track_id),
                            },
                        )
                        .is_ok()
                    {
                        decoder.reset();
                        audio_buf_d.lock().unwrap_or_else(|e| e.into_inner()).clear();
                        seek_base_d.store(target_ms, Ordering::Relaxed);
                        consumed_d.store(0, Ordering::Relaxed);
                    }
                }

                // Backpressure
                loop {
                    if stop_d.load(Ordering::Relaxed) {
                        if cleanup_d {
                            let _ = fs::remove_file(&path_d);
                        }
                        return;
                    }
                    if seek_req_d.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
                        break;
                    }
                    let len = audio_buf_d.lock().unwrap_or_else(|e| e.into_inner()).len();
                    if len < max_buf_samples {
                        break;
                    }
                    thread::sleep(Duration::from_millis(8));
                }

                match format.next_packet() {
                    Ok(packet) => {
                        if packet.track_id() != track_id {
                            continue;
                        }
                        match decoder.decode(&packet) {
                            Ok(decoded) => {
                                let spec = *decoded.spec();
                                let mut sbuf =
                                    SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
                                sbuf.copy_interleaved_ref(decoded);
                                let samples = sbuf.samples();

                                // Update FFT mono ring buffer
                                if !tel_d.is_paused.load(Ordering::Relaxed) {
                                    let mono: Vec<f32> = if file_ch >= 2 {
                                        samples
                                            .chunks(file_ch)
                                            .map(|c| (c[0] + c[1]) * 0.5)
                                            .collect()
                                    } else {
                                        samples.to_vec()
                                    };
                                    if !mono.is_empty() {
                                        let mut sb =
                                            tel_d.spec_buf.lock().unwrap_or_else(|e| e.into_inner());
                                        let n = mono.len().min(sb.len());
                                        let len = sb.len();
                                        sb.rotate_left(n);
                                        sb[len - n..].copy_from_slice(&mono[mono.len() - n..]);
                                    }
                                }

                                let converted = convert_audio(
                                    samples, file_ch, device_ch, file_sr, device_sr,
                                );
                                audio_buf_d
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .extend(converted);
                            }
                            Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
                            Err(_) => break,
                        }
                    }
                    Err(_) => break,
                }
            }

            decode_fin_d.store(true, Ordering::Relaxed);
            // Wait until audio buffer drains to notify natural end of track
            loop {
                if stop_d.load(Ordering::Relaxed) {
                    break;
                }
                if audio_buf_d
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_empty()
                {
                    if tel_d.generation.load(Ordering::Relaxed) == gen_d {
                        tel_d.is_playing.store(false, Ordering::Relaxed);
                        tel_d.track_ended_generation.store(gen_d, Ordering::Relaxed);
                    }
                    break;
                }
                thread::sleep(Duration::from_millis(30));
            }
            if cleanup_d {
                let _ = fs::remove_file(&path_d);
            }
        });
    }

    // cpal playback stream
    let audio_buf_c = audio_buf.clone();
    let volume_c = volume.clone();
    let tel_c = prep.telemetry.clone();
    let seek_base_c = seek_base_ms.clone();
    let consumed_c = consumed_device_frames.clone();
    let stop_c = prep.stop.clone();

    let stream = device.build_output_stream::<f32, _, _>(
        &cpal_cfg,
        move |data: &mut [f32], _| {
            if stop_c.load(Ordering::Relaxed) || tel_c.is_paused.load(Ordering::Relaxed) {
                data.fill(0.0);
                return;
            }
            let vol = *volume_c.lock().unwrap_or_else(|e| e.into_inner());
            let mut buf = audio_buf_c.lock().unwrap_or_else(|e| e.into_inner());
            let mut popped_samples = 0u64;
            for sample in data.iter_mut() {
                if let Some(s) = buf.pop_front() {
                    *sample = s * vol;
                    popped_samples += 1;
                } else {
                    *sample = 0.0;
                }
            }
            if popped_samples > 0 {
                let frames = popped_samples / device_ch.max(1) as u64;
                let total_frames = consumed_c.fetch_add(frames, Ordering::Relaxed) + frames;
                let pos = seek_base_c.load(Ordering::Relaxed)
                    + (total_frames * 1000) / device_sr.max(1) as u64;
                tel_c.position_ms.store(pos, Ordering::Relaxed);
            }
        },
        |_err| {},
        None,
    )?;

    stream.play()?;
    Ok(stream)
}

fn load_mono_f32(path: &Path, target_sr: u32, stop: &AtomicBool) -> Result<Vec<f32>> {
    let file = fs::File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let probed = symphonia::default::get_probe().format(
        &Hint::new(),
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    )?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| anyhow!("No track"))?
        .clone();
    let file_sr = track.codec_params.sample_rate.unwrap_or(44100);
    let ch = track.codec_params.channels.map(|c| c.count()).unwrap_or(2);
    let mut decoder =
        symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;

    let mut samples: Vec<f32> = Vec::with_capacity(file_sr as usize * 180);
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(anyhow!("stopped"));
        }
        match format.next_packet() {
            Ok(packet) => match decoder.decode(&packet) {
                Ok(decoded) => {
                    let mut sbuf = SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
                    sbuf.copy_interleaved_ref(decoded);
                    let s = sbuf.samples();
                    if ch >= 2 {
                        samples.extend(s.chunks(ch).map(|c| (c[0] + c[1]) * 0.5));
                    } else {
                        samples.extend_from_slice(s);
                    }
                }
                Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
                Err(_) => break,
            },
            Err(_) => break,
        }
    }

    if file_sr != target_sr && !samples.is_empty() {
        let ratio = file_sr as f64 / target_sr as f64;
        let new_len = (samples.len() as f64 / ratio) as usize;
        samples = (0..new_len)
            .map(|i| {
                let idx = ((i as f64 * ratio) as usize).min(samples.len() - 1);
                samples[idx]
            })
            .collect();
    }
    Ok(samples)
}

fn convert_audio(
    input: &[f32],
    file_ch: usize,
    device_ch: usize,
    file_sr: u32,
    device_sr: u32,
) -> Vec<f32> {
    let file_ch = file_ch.max(1);
    let device_ch = device_ch.max(1);
    let frames_in = input.len() / file_ch;
    if frames_in == 0 {
        return Vec::new();
    }

    let converted_ch: Vec<f32> = if file_ch == device_ch {
        input.to_vec()
    } else if device_ch == 1 {
        (0..frames_in)
            .map(|f| (0..file_ch).map(|c| input[f * file_ch + c]).sum::<f32>() / file_ch as f32)
            .collect()
    } else if file_ch == 1 {
        let mut out = Vec::with_capacity(frames_in * device_ch);
        for f in 0..frames_in {
            for _ in 0..device_ch {
                out.push(input[f]);
            }
        }
        out
    } else {
        let mut out = Vec::with_capacity(frames_in * device_ch);
        for f in 0..frames_in {
            for dc in 0..device_ch {
                out.push(input[f * file_ch + dc.min(file_ch - 1)]);
            }
        }
        out
    };

    if file_sr == device_sr {
        return converted_ch;
    }

    let ratio = file_sr as f64 / device_sr as f64;
    let frames_out = (frames_in as f64 / ratio).ceil() as usize;
    let mut out = Vec::with_capacity(frames_out * device_ch);
    for i in 0..frames_out {
        let pos = i as f64 * ratio;
        let idx0 = (pos as usize).min(frames_in.saturating_sub(1));
        let idx1 = (idx0 + 1).min(frames_in.saturating_sub(1));
        let frac = (pos - idx0 as f64) as f32;
        for c in 0..device_ch {
            let s0 = converted_ch[idx0 * device_ch + c];
            let s1 = converted_ch[idx1 * device_ch + c];
            out.push(s0 + (s1 - s0) * frac);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_audio_preserves_mono_to_stereo_and_resamples() {
        let mono = vec![0.5f32; 100];
        let stereo = convert_audio(&mono, 1, 2, 44100, 44100);
        assert_eq!(stereo.len(), 200);
        assert!((stereo[0] - 0.5).abs() < 1e-6);

        let resampled = convert_audio(&stereo, 2, 2, 44100, 48000);
        assert!(resampled.len() > 200);
    }
}
