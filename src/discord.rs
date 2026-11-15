use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

pub const DEFAULT_DISCORD_CLIENT_ID: &str = "1130698654987067493";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordPresence {
    pub track_id: u64,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub cover_id: Option<String>,
    pub duration_secs: u64,
    pub elapsed_secs: u64,
    pub is_paused: bool,
}

enum RpcCmd {
    Update(Option<DiscordPresence>),
    Shutdown,
}

pub struct DiscordRpcBridge {
    tx: Option<Sender<RpcCmd>>,
    handle: Option<JoinHandle<()>>,
}

impl DiscordRpcBridge {
    pub fn new(headless: bool, client_id: String) -> Self {
        if headless {
            return Self {
                tx: None,
                handle: None,
            };
        }

        let effective_client_id = if client_id.trim().is_empty() {
            DEFAULT_DISCORD_CLIENT_ID.to_string()
        } else {
            client_id.trim().to_string()
        };

        let (tx, rx) = mpsc::channel();
        let handle = thread::Builder::new()
            .name("discord-rpc".into())
            .spawn(move || rpc_worker_loop(rx, effective_client_id))
            .ok();

        if handle.is_none() {
            return Self {
                tx: None,
                handle: None,
            };
        }

        Self {
            tx: Some(tx),
            handle,
        }
    }

    pub fn update(&self, presence: Option<DiscordPresence>) {
        if let Some(ref tx) = self.tx {
            let _ = tx.send(RpcCmd::Update(presence));
        }
    }

    pub fn clear(&self) {
        self.update(None);
    }

    pub fn shutdown(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(RpcCmd::Shutdown);
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for DiscordRpcBridge {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn clamp_discord_str(input: &str, fallback: &str) -> String {
    let trimmed = input.trim();
    let base = if trimmed.is_empty() {
        fallback.trim()
    } else {
        trimmed
    };
    let mut out: String = base.chars().take(128).collect();
    if out.chars().count() < 2 {
        if out.is_empty() {
            out = fallback.chars().take(128).collect();
        }
        if out.chars().count() < 2 {
            out.push(' ');
        }
    }
    out
}

pub fn encode_ipc_frame(opcode: u32, payload: &str) -> Vec<u8> {
    let bytes = payload.as_bytes();
    let mut frame = Vec::with_capacity(8 + bytes.len());
    frame.extend_from_slice(&opcode.to_le_bytes());
    frame.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    frame.extend_from_slice(bytes);
    frame
}

pub fn build_set_activity_json(
    pid: u32,
    nonce: u64,
    presence: Option<&DiscordPresence>,
    now_secs: u64,
) -> Value {
    let activity = match presence {
        Some(p) => {
            let details = clamp_discord_str(&p.title, "Unknown Track");
            let state = if p.is_paused {
                if p.artist.trim().is_empty() {
                    clamp_discord_str("Paused", "Paused")
                } else {
                    clamp_discord_str(&format!("by {} (Paused)", p.artist.trim()), "Paused")
                }
            } else if p.artist.trim().is_empty() {
                clamp_discord_str("TIDAL", "TIDAL")
            } else {
                clamp_discord_str(&format!("by {}", p.artist.trim()), "TIDAL")
            };

            let large_image = match p.cover_id {
                Some(ref cid) if !cid.trim().is_empty() => {
                    format!(
                        "https://resources.tidal.com/images/{}/640x640.jpg",
                        cid.trim().replace('-', "/")
                    )
                }
                _ => "tidal".to_string(),
            };
            let large_text = clamp_discord_str(&p.album, "TIDAL");

            let mut obj = json!({
                "type": 2,
                "details": details,
                "state": state,
                "assets": {
                    "large_image": large_image,
                    "large_text": large_text,
                },
            });

            if !p.is_paused {
                let start = now_secs.saturating_sub(p.elapsed_secs);
                let ts = if p.duration_secs > 0 {
                    json!({
                        "start": start,
                        "end": start + p.duration_secs,
                    })
                } else {
                    json!({
                        "start": start,
                    })
                };
                obj["timestamps"] = ts;
            }

            if p.track_id > 0 {
                obj["buttons"] = json!([{
                    "label": "Listen on TIDAL",
                    "url": format!("https://tidal.com/browse/track/{}", p.track_id),
                }]);
            }

            obj
        }
        None => Value::Null,
    };

    json!({
        "cmd": "SET_ACTIVITY",
        "args": {
            "pid": pid,
            "activity": activity,
        },
        "nonce": nonce.to_string(),
    })
}

fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(unix)]
fn rpc_worker_loop(rx: mpsc::Receiver<RpcCmd>, client_id: String) {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;

    fn candidate_sockets() -> Vec<PathBuf> {
        let mut bases = Vec::new();
        for key in ["XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"] {
            if let Ok(val) = std::env::var(key) {
                if !val.is_empty() {
                    let p = PathBuf::from(val);
                    if !bases.contains(&p) {
                        bases.push(p);
                    }
                }
            }
        }
        let tmp = PathBuf::from("/tmp");
        if !bases.contains(&tmp) {
            bases.push(tmp);
        }

        let prefixes = [
            "discord-ipc-",
            "app/com.discordapp.Discord/discord-ipc-",
            "app/dev.vencord.Vesktop/discord-ipc-",
            "snap.discord/discord-ipc-",
            "vesktop-ipc-",
        ];

        let mut sockets = Vec::new();
        for base in &bases {
            for prefix in &prefixes {
                for i in 0..10 {
                    sockets.push(base.join(format!("{}{}", prefix, i)));
                }
            }
        }
        sockets
    }

    fn read_ipc_frame(stream: &mut UnixStream) -> std::io::Result<u32> {
        let mut hdr = [0u8; 8];
        stream.read_exact(&mut hdr)?;
        let opcode = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
        let len = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as usize;
        let capped = len.min(1024 * 1024);
        let mut buf = vec![0u8; capped];
        if capped > 0 {
            stream.read_exact(&mut buf)?;
        }
        Ok(opcode)
    }

    fn connect_ipc(client_id: &str) -> Option<UnixStream> {
        for sock_path in candidate_sockets() {
            if !sock_path.exists() {
                continue;
            }
            let Ok(mut stream) = UnixStream::connect(&sock_path) else {
                continue;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));

            let handshake = json!({
                "v": 1,
                "client_id": client_id,
            })
            .to_string();
            let frame = encode_ipc_frame(0, &handshake);
            if stream.write_all(&frame).is_err() {
                continue;
            }
            if read_ipc_frame(&mut stream).is_err() {
                continue;
            }
            return Some(stream);
        }
        None
    }

    fn send_activity(
        stream: &mut UnixStream,
        pid: u32,
        nonce: &mut u64,
        presence: Option<&DiscordPresence>,
    ) -> bool {
        *nonce = nonce.wrapping_add(1);
        let payload = build_set_activity_json(pid, *nonce, presence, unix_now_secs()).to_string();
        let frame = encode_ipc_frame(1, &payload);
        if stream.write_all(&frame).is_err() {
            return false;
        }
        // Drain response frame so socket buffer does not grow across track updates
        match read_ipc_frame(stream) {
            Ok(_) => true,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                true
            }
            Err(_) => false,
        }
    }

    let pid = std::process::id();
    let mut nonce: u64 = 0;
    let mut stream: Option<UnixStream> = None;
    let mut latest: Option<Option<DiscordPresence>> = None;

    loop {
        let cmd = if stream.is_none() && matches!(latest, Some(Some(_))) {
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(c) => Some(c),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match rx.recv() {
                Ok(c) => Some(c),
                Err(_) => break,
            }
        };

        if let Some(c) = cmd {
            match c {
                RpcCmd::Update(p) => latest = Some(p),
                RpcCmd::Shutdown => break,
            }
            while let Ok(next) = rx.try_recv() {
                match next {
                    RpcCmd::Update(p) => latest = Some(p),
                    RpcCmd::Shutdown => {
                        if let Some(ref mut s) = stream {
                            let _ = send_activity(s, pid, &mut nonce, None);
                            let close_frame = encode_ipc_frame(2, "{}");
                            let _ = s.write_all(&close_frame);
                        }
                        return;
                    }
                }
            }
        }

        let Some(ref target_presence) = latest else {
            continue;
        };

        if stream.is_none() {
            if target_presence.is_none() {
                continue;
            }
            stream = connect_ipc(&client_id);
        }

        if let Some(ref mut s) = stream {
            if !send_activity(s, pid, &mut nonce, target_presence.as_ref()) {
                stream = None;
            }
        }
    }

    if let Some(ref mut s) = stream {
        let _ = send_activity(s, pid, &mut nonce, None);
        let close_frame = encode_ipc_frame(2, "{}");
        let _ = s.write_all(&close_frame);
    }
}

#[cfg(not(unix))]
fn rpc_worker_loop(rx: mpsc::Receiver<RpcCmd>, _client_id: String) {
    while let Ok(cmd) = rx.recv() {
        if matches!(cmd, RpcCmd::Shutdown) {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_valid_set_activity_payload_and_frame_header() {
        let p = DiscordPresence {
            track_id: 123456,
            title: "Midnight Tide".into(),
            artist: "KATSEYE".into(),
            album: "Soft Is Strong".into(),
            cover_id: Some("abcd-1234-ef56".into()),
            duration_secs: 200,
            elapsed_secs: 45,
            is_paused: false,
        };

        let val = build_set_activity_json(42, 7, Some(&p), 1_700_000_000);
        assert_eq!(val["cmd"], "SET_ACTIVITY");
        assert_eq!(val["args"]["pid"], 42);
        assert_eq!(val["nonce"], "7");
        assert_eq!(val["args"]["activity"]["type"], 2);
        assert_eq!(val["args"]["activity"]["details"], "Midnight Tide");
        assert_eq!(val["args"]["activity"]["state"], "by KATSEYE");
        assert_eq!(
            val["args"]["activity"]["timestamps"]["start"],
            1_700_000_000 - 45
        );
        assert_eq!(
            val["args"]["activity"]["timestamps"]["end"],
            1_700_000_000 - 45 + 200
        );
        assert_eq!(
            val["args"]["activity"]["assets"]["large_image"],
            "https://resources.tidal.com/images/abcd/1234/ef56/640x640.jpg"
        );
        assert_eq!(
            val["args"]["activity"]["assets"]["large_text"],
            "Soft Is Strong"
        );
        assert_eq!(
            val["args"]["activity"]["buttons"][0]["url"],
            "https://tidal.com/browse/track/123456"
        );

        let mut paused_p = p.clone();
        paused_p.is_paused = true;
        let paused_val = build_set_activity_json(42, 8, Some(&paused_p), 1_700_000_000);
        assert_eq!(paused_val["args"]["activity"]["state"], "by KATSEYE (Paused)");
        assert!(paused_val["args"]["activity"]["timestamps"].is_null());

        let cleared_val = build_set_activity_json(42, 9, None, 1_700_000_000);
        assert!(cleared_val["args"]["activity"].is_null());

        let json_str = val.to_string();
        let frame = encode_ipc_frame(1, &json_str);
        assert_eq!(&frame[0..4], &1u32.to_le_bytes());
        assert_eq!(&frame[4..8], &(json_str.len() as u32).to_le_bytes());
        assert_eq!(&frame[8..], json_str.as_bytes());
    }
}
