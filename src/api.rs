use anyhow::{anyhow, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use crate::auth::{self, Session, TIDAL_UA};
use crate::config::AudioQuality;

const API_BASE: &str = "https://api.tidal.com/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FavoriteKind {
    Track,
    Album,
    Artist,
    Playlist,
}

impl FavoriteKind {
    fn path_segment(self) -> &'static str {
        match self {
            Self::Track => "tracks",
            Self::Album => "albums",
            Self::Artist => "artists",
            Self::Playlist => "playlists",
        }
    }

    fn form_key(self) -> &'static str {
        match self {
            Self::Track => "trackId",
            Self::Album => "albumId",
            Self::Artist => "artistId",
            Self::Playlist => "uuids",
        }
    }
}

#[derive(Debug, Clone)]
pub struct EncryptionInfo {
    pub key: [u8; 16],
    pub nonce: [u8; 8],
}

#[derive(Debug, Clone)]
pub struct ResolvedStream {
    pub urls: Vec<String>,
    pub encryption: Option<EncryptionInfo>,
    pub actual_quality: String,
    pub codecs: String,
    pub extension: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackInfo {
    pub id: u64,
    pub title: String,
    pub artist_name: String,
    pub artist_id: Option<u64>,
    pub artists: Vec<String>,
    pub album_name: String,
    pub album_id: Option<u64>,
    pub album_cover: Option<String>,
    pub album_artist: String,
    pub album_copyright: Option<String>,
    pub album_release_year: Option<i32>,
    pub track_num: u32,
    pub volume_num: u32,
    pub isrc: Option<String>,
    pub audio_quality: String,
    pub duration: u64,
    pub explicit: bool,
    pub hires: bool,
    /// Present when this TrackInfo represents a local file on disk.
    pub local_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlbumInfo {
    pub id: u64,
    pub title: String,
    pub artist_name: String,
    pub artist_id: Option<u64>,
    pub cover: Option<String>,
    pub release_year: Option<String>,
    pub track_count: u32,
    pub duration: u64,
    pub audio_quality: String,
    pub explicit: bool,
    pub copyright: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtistInfo {
    pub id: u64,
    pub name: String,
    pub picture: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistInfo {
    pub uuid: String,
    pub title: String,
    pub description: String,
    pub creator: String,
    pub cover: Option<String>,
    pub track_count: u32,
    pub duration: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MixInfo {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub cover: Option<String>,
}

#[derive(Debug, Clone)]
pub enum HomeItems {
    Tracks(Vec<TrackInfo>),
    Albums(Vec<AlbumInfo>),
    Playlists(Vec<PlaylistInfo>),
    Mixes(Vec<MixInfo>),
}

#[derive(Debug, Clone)]
pub struct HomeSection {
    pub title: String,
    pub items: HomeItems,
}

#[derive(Debug, Clone, Default)]
pub struct SearchResults {
    pub query: String,
    pub tracks: Vec<TrackInfo>,
    pub albums: Vec<AlbumInfo>,
    pub artists: Vec<ArtistInfo>,
    pub playlists: Vec<PlaylistInfo>,
}

#[derive(Debug, Clone, Default)]
pub struct LibraryData {
    pub tracks: Vec<TrackInfo>,
    pub albums: Vec<AlbumInfo>,
    pub artists: Vec<ArtistInfo>,
    pub playlists: Vec<PlaylistInfo>,
    pub mixes: Vec<MixInfo>,
    pub fav_track_ids: HashSet<u64>,
    pub fav_album_ids: HashSet<u64>,
    pub fav_artist_ids: HashSet<u64>,
    pub fav_playlist_uuids: HashSet<String>,
}

#[derive(Debug, Clone)]
pub struct ArtistDetail {
    pub artist: ArtistInfo,
    pub top_tracks: Vec<TrackInfo>,
    pub albums: Vec<AlbumInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TrackLyrics {
    pub lyrics: Option<String>,
    pub subtitles: Option<String>,
}

#[derive(Clone)]
pub struct TidalClient {
    session: Arc<Mutex<Session>>,
    http: reqwest::blocking::Client,
}

impl TidalClient {
    pub fn new(session: Session) -> Self {
        let http = reqwest::blocking::Client::builder()
            .user_agent(TIDAL_UA)
            .timeout(std::time::Duration::from_secs(25))
            .build()
            .unwrap_or_default();
        Self {
            session: Arc::new(Mutex::new(session)),
            http,
        }
    }

    pub fn session_snapshot(&self) -> Session {
        self.session.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn update_session(&self, new_session: Session) {
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) = new_session;
    }

    fn ensure_fresh_session(&self) -> Session {
        let cur = self.session_snapshot();
        if cur.is_expired() && !cur.refresh_token.is_empty() {
            if let Ok(mut refreshed) = auth::refresh_token(&cur.refresh_token) {
                if refreshed.user_id == 0 {
                    refreshed.user_id = cur.user_id;
                }
                if refreshed.country_code.is_empty() {
                    refreshed.country_code = cur.country_code.clone();
                }
                if refreshed.user_name.is_none() {
                    refreshed.user_name = cur.user_name.clone();
                }
                let _ = auth::save_session(&refreshed);
                self.update_session(refreshed.clone());
                return refreshed;
            }
        }
        cur
    }

    fn try_refresh_after_401(&self) -> bool {
        let cur = self.session_snapshot();
        if cur.refresh_token.is_empty() {
            return false;
        }
        if let Ok(mut refreshed) = auth::refresh_token(&cur.refresh_token) {
            if refreshed.user_id == 0 {
                refreshed.user_id = cur.user_id;
            }
            if refreshed.country_code.is_empty() {
                refreshed.country_code = cur.country_code.clone();
            }
            if refreshed.user_name.is_none() {
                refreshed.user_name = cur.user_name.clone();
            }
            let _ = auth::save_session(&refreshed);
            self.update_session(refreshed);
            true
        } else {
            false
        }
    }

    fn request_json(&self, method: reqwest::Method, path: &str, params: &[(&str, &str)], form: Option<&[(&str, &str)]>) -> Result<Value> {
        for attempt in 0..2 {
            let sess = self.ensure_fresh_session();
            let url = format!("{}/{}", API_BASE, path);
            let mut req = self
                .http
                .request(method.clone(), &url)
                .header("Authorization", sess.auth_header());

            let mut has_country = false;
            for &(k, v) in params {
                if k == "countryCode" {
                    has_country = true;
                }
                req = req.query(&[(k, v)]);
            }
            if !has_country && !sess.country_code.is_empty() {
                req = req.query(&[("countryCode", sess.country_code.as_str())]);
            }
            if let Some(f) = form {
                req = req.form(f);
            }

            let resp = req.send()?;
            let status = resp.status();
            if status.as_u16() == 401 && attempt == 0 && self.try_refresh_after_401() {
                continue;
            }
            if !status.is_success() {
                let body = resp.text().unwrap_or_default();
                return Err(anyhow!("{}", parse_api_error(&body, status.as_u16())));
            }
            let text = resp.text()?;
            if text.trim().is_empty() {
                return Ok(Value::Object(Default::default()));
            }
            let val: Value = serde_json::from_str(&text)
                .map_err(|e| anyhow!("Invalid JSON from Tidal on {}: {}", path, e))?;
            return Ok(val);
        }
        Err(anyhow!("Session expired — please sign in again (Ctrl+L)"))
    }

    fn get_json(&self, path: &str, params: &[(&str, &str)]) -> Result<Value> {
        self.request_json(reqwest::Method::GET, path, params, None)
    }

    pub fn fetch_user_profile(&self) -> Result<Session> {
        let sess_val = self.get_json("sessions", &[])?;
        let mut sess = self.session_snapshot();
        if let Some(uid) = sess_val.get("userId").and_then(|v| v.as_u64()) {
            if uid > 0 {
                sess.user_id = uid;
            }
        }
        if let Some(cc) = sess_val.get("countryCode").and_then(|v| v.as_str()) {
            if !cc.is_empty() {
                sess.country_code = cc.to_string();
            }
        }
        if let Some(sid) = sess_val.get("sessionId").and_then(|v| v.as_str()) {
            sess.session_id = Some(sid.to_string());
        }
        if sess.user_id > 0 {
            if let Ok(user_val) = self.get_json(&format!("users/{}", sess.user_id), &[]) {
                let uname = user_val
                    .get("username")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .or_else(|| {
                        let f = user_val.get("firstName").and_then(|v| v.as_str()).unwrap_or("");
                        let l = user_val.get("lastName").and_then(|v| v.as_str()).unwrap_or("");
                        let combined = format!("{} {}", f, l).trim().to_string();
                        if combined.is_empty() { None } else { Some(combined) }
                    });
                if uname.is_some() {
                    sess.user_name = uname;
                }
            }
        }
        self.update_session(sess.clone());
        let _ = auth::save_session(&sess);
        Ok(sess)
    }

    pub fn home_feed(&self) -> Result<Vec<HomeSection>> {
        let mut sections = Vec::new();
        if let Ok(home) = self.get_json("pages/home", &[("deviceType", "DESKTOP")]) {
            sections.extend(parse_home_page(&home));
        }
        if let Ok(mixes) = self.mixes() {
            if !mixes.is_empty() {
                sections.insert(
                    0,
                    HomeSection {
                        title: "My Mixes".to_string(),
                        items: HomeItems::Mixes(mixes),
                    },
                );
            }
        }
        Ok(sections)
    }

    pub fn search_all(&self, query: &str, limit: u32) -> Result<SearchResults> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(SearchResults::default());
        }
        let limit_s = limit.to_string();
        let val = self.get_json(
            "search",
            &[
                ("query", q),
                ("types", "TRACKS,ALBUMS,ARTISTS,PLAYLISTS"),
                ("limit", &limit_s),
            ],
        )?;
        Ok(parse_search_results(q, &val))
    }

    pub fn mixes(&self) -> Result<Vec<MixInfo>> {
        let val = self.get_json(
            "pages/my_collection_my_mixes",
            &[("deviceType", "BROWSER"), ("locale", "en_US")],
        )?;
        Ok(parse_mixes_page(&val))
    }

    pub fn mix_tracks(&self, mix_id: &str) -> Result<Vec<TrackInfo>> {
        let val = self.get_json(
            &format!("mixes/{}/items", mix_id),
            &[("limit", "50"), ("deviceType", "BROWSER")],
        )?;
        Ok(extract_wrapped_tracks(&val))
    }

    pub fn album_detail(&self, album_id: u64) -> Result<(AlbumInfo, Vec<TrackInfo>)> {
        let a_val = self.get_json(&format!("albums/{}", album_id), &[])?;
        let album = normalize_album(&a_val)
            .ok_or_else(|| anyhow!("Could not parse album {}", album_id))?;

        let tracks = match self.get_json(&format!("albums/{}/items", album_id), &[("limit", "100")]) {
            Ok(v) => {
                let mut list = extract_wrapped_tracks(&v);
                if list.is_empty() {
                    if let Some(arr) = v.get("items").and_then(|x| x.as_array()) {
                        list = arr.iter().filter_map(normalize_track).collect();
                    }
                }
                list
            }
            Err(_) => {
                let v = self.get_json(&format!("albums/{}/tracks", album_id), &[("limit", "100")])?;
                v.get("items")
                    .and_then(|x| x.as_array())
                    .map(|arr| arr.iter().filter_map(normalize_track).collect())
                    .unwrap_or_default()
            }
        };
        Ok((album, tracks))
    }

    pub fn playlist_detail(&self, uuid: &str) -> Result<(PlaylistInfo, Vec<TrackInfo>)> {
        let p_val = self.get_json(&format!("playlists/{}", uuid), &[])?;
        let playlist = normalize_playlist(&p_val)
            .ok_or_else(|| anyhow!("Could not parse playlist {}", uuid))?;
        let items_val = self.get_json(&format!("playlists/{}/items", uuid), &[("limit", "100")])?;
        let tracks = extract_wrapped_tracks(&items_val);
        Ok((playlist, tracks))
    }

    pub fn artist_detail(&self, artist_id: u64) -> Result<ArtistDetail> {
        let a_val = self.get_json(&format!("artists/{}", artist_id), &[])?;
        let artist = normalize_artist(&a_val)
            .ok_or_else(|| anyhow!("Could not parse artist {}", artist_id))?;

        let top_val = self
            .get_json(&format!("artists/{}/toptracks", artist_id), &[("limit", "15")])
            .unwrap_or(Value::Null);
        let top_tracks = top_val
            .get("items")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(normalize_track).collect())
            .unwrap_or_default();

        let albums_val = self
            .get_json(&format!("artists/{}/albums", artist_id), &[("limit", "30")])
            .unwrap_or(Value::Null);
        let albums = albums_val
            .get("items")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(normalize_album).collect())
            .unwrap_or_default();

        Ok(ArtistDetail {
            artist,
            top_tracks,
            albums,
        })
    }

    pub fn track_radio(&self, track_id: u64) -> Result<Vec<TrackInfo>> {
        let val = self.get_json(&format!("tracks/{}/radio", track_id), &[("limit", "50")])?;
        Ok(val
            .get("items")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(normalize_track).collect())
            .unwrap_or_default())
    }

    pub fn lyrics(&self, track_id: u64) -> Result<TrackLyrics> {
        let val = self.get_json(&format!("tracks/{}/lyrics", track_id), &[])?;
        Ok(serde_json::from_value(val)?)
    }

    pub fn library(&self) -> Result<LibraryData> {
        let sess = self.ensure_fresh_session();
        if sess.user_id == 0 {
            return Err(anyhow!("No Tidal user ID — please sign in (Ctrl+L)"));
        }
        let uid = sess.user_id;
        let mut lib = LibraryData::default();

        if let Ok(v) = self.get_json(&format!("users/{}/favorites/tracks", uid), &[("limit", "100")]) {
            for item in extract_wrapped_items(&v) {
                if let Some(t) = normalize_track(item) {
                    lib.fav_track_ids.insert(t.id);
                    lib.tracks.push(t);
                }
            }
        }

        if let Ok(v) = self.get_json(&format!("users/{}/favorites/albums", uid), &[("limit", "100")]) {
            for item in extract_wrapped_items(&v) {
                if let Some(a) = normalize_album(item) {
                    lib.fav_album_ids.insert(a.id);
                    lib.albums.push(a);
                }
            }
        }

        if let Ok(v) = self.get_json(&format!("users/{}/favorites/artists", uid), &[("limit", "100")]) {
            for item in extract_wrapped_items(&v) {
                if let Some(a) = normalize_artist(item) {
                    lib.fav_artist_ids.insert(a.id);
                    lib.artists.push(a);
                }
            }
        }

        let mut seen_playlists = HashSet::new();
        if let Ok(v) = self.get_json(&format!("users/{}/favorites/playlists", uid), &[("limit", "100")]) {
            for item in extract_wrapped_items(&v) {
                if let Some(p) = normalize_playlist(item) {
                    if seen_playlists.insert(p.uuid.clone()) {
                        lib.fav_playlist_uuids.insert(p.uuid.clone());
                        lib.playlists.push(p);
                    }
                }
            }
        }
        if let Ok(v) = self.get_json(&format!("users/{}/playlists", uid), &[("limit", "50")]) {
            if let Some(arr) = v.get("items").and_then(|x| x.as_array()) {
                for item in arr {
                    if let Some(p) = normalize_playlist(item) {
                        if seen_playlists.insert(p.uuid.clone()) {
                            lib.playlists.push(p);
                        }
                    }
                }
            }
        }

        if let Ok(mixes) = self.mixes() {
            lib.mixes = mixes;
        }

        Ok(lib)
    }

    pub fn toggle_favorite(&self, kind: FavoriteKind, id: &str, currently_fav: bool) -> Result<bool> {
        let sess = self.ensure_fresh_session();
        if sess.user_id == 0 {
            return Err(anyhow!("Sign in to save favorites"));
        }
        let base = format!("users/{}/favorites/{}", sess.user_id, kind.path_segment());
        if currently_fav {
            self.request_json(reqwest::Method::DELETE, &format!("{}/{}", base, id), &[], None)?;
            Ok(false)
        } else {
            self.request_json(
                reqwest::Method::POST,
                &base,
                &[],
                Some(&[(kind.form_key(), id)]),
            )?;
            Ok(true)
        }
    }

    pub fn resolve_stream(&self, track_id: u64, preferred: AudioQuality) -> Result<ResolvedStream> {
        let mut last_err = anyhow!("Unable to resolve Tidal stream");
        for &tier in preferred.fallback_chain() {
            match self.resolve_stream_exact(track_id, tier) {
                Ok(stream) => return Ok(stream),
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("DRM") || msg.contains("Sign in") || msg.contains("expired") {
                        return Err(e);
                    }
                    last_err = e;
                }
            }
        }
        Err(last_err)
    }

    pub fn resolve_stream_exact(&self, track_id: u64, quality: &str) -> Result<ResolvedStream> {
        let val = self.get_json(
            &format!("tracks/{}/playbackinfopostpaywall", track_id),
            &[
                ("audioquality", quality),
                ("playbackmode", "STREAM"),
                ("assetpresentation", "FULL"),
                ("prefetchlevel", "NONE"),
            ],
        )?;

        #[derive(Deserialize)]
        struct PlaybackInfo {
            #[serde(rename = "manifestMimeType")]
            manifest_mime_type: String,
            manifest: String,
            #[serde(rename = "audioQuality")]
            audio_quality: Option<String>,
        }

        let info: PlaybackInfo = serde_json::from_value(val)?;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&info.manifest)
            .map_err(|e| anyhow!("Manifest base64 decode failed: {}", e))?;
        let actual_quality = info
            .audio_quality
            .filter(|q| !q.is_empty())
            .unwrap_or_else(|| quality.to_string());

        if info.manifest_mime_type.contains("bts") {
            #[derive(Deserialize)]
            struct BtsManifest {
                urls: Vec<String>,
                #[serde(default)]
                codecs: String,
                #[serde(rename = "encryptionType", default)]
                encryption_type: String,
                #[serde(rename = "keyId")]
                key_id: Option<String>,
            }

            let bts: BtsManifest = serde_json::from_slice(&decoded)
                .map_err(|e| anyhow!("BTS manifest parse failed: {}", e))?;
            if bts.urls.is_empty() {
                return Err(anyhow!("Tidal returned an empty stream URL list"));
            }
            let encryption = if bts.encryption_type == "OLD_AES" {
                let key_id = bts
                    .key_id
                    .ok_or_else(|| anyhow!("Missing keyId for encrypted stream"))?;
                Some(decrypt_security_token(&key_id)?)
            } else if !bts.encryption_type.is_empty() && bts.encryption_type != "NONE" {
                return Err(anyhow!("Track is DRM protected ({})", bts.encryption_type));
            } else {
                None
            };
            let extension = extension_for_codecs(&bts.codecs).to_string();
            return Ok(ResolvedStream {
                urls: bts.urls,
                encryption,
                actual_quality,
                codecs: bts.codecs,
                extension,
            });
        }

        if info.manifest_mime_type.contains("dash") {
            let xml = String::from_utf8_lossy(&decoded);
            let (urls, codecs) = parse_dash_manifest(&xml)?;
            let extension = extension_for_codecs(&codecs).to_string();
            return Ok(ResolvedStream {
                urls,
                encryption: None,
                actual_quality,
                codecs,
                extension,
            });
        }

        Err(anyhow!("Unsupported stream manifest type: {}", info.manifest_mime_type))
    }

    pub fn fetch_cover(&self, cover_id: &str, size: u32) -> Result<Vec<u8>> {
        let path = cover_id.replace('-', "/");
        let url = format!("https://resources.tidal.com/images/{}/{}x{}.jpg", path, size, size);
        let bytes = self.http.get(&url).send()?.error_for_status()?.bytes()?;
        Ok(bytes.to_vec())
    }
}

fn parse_api_error(body: &str, status: u16) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        for key in ["userMessage", "error_description", "error"] {
            if let Some(msg) = v.get(key).and_then(|s| s.as_str()) {
                if !msg.is_empty() {
                    return msg.to_string();
                }
            }
        }
    }
    match status {
        401 => "Tidal session expired. Press Ctrl+L to sign in again.".to_string(),
        403 => "Tidal says this account cannot access that item.".to_string(),
        404 => "Item not found on Tidal.".to_string(),
        s if s >= 500 => format!("Tidal server error ({}).", s),
        s => format!("Tidal API error ({}).", s),
    }
}

pub fn extension_for_codecs(codecs: &str) -> &'static str {
    let c = codecs.to_lowercase();
    if c.contains("flac") {
        "flac"
    } else if c.contains("ec-3") || c.contains("eac3") || c.contains("ac-4") || c.contains("ac4") {
        "mp4"
    } else {
        "m4a"
    }
}

/// Extract wrapped `{ item: ... }` arrays or direct item arrays returned by Tidal endpoints.
fn extract_wrapped_items(val: &Value) -> Vec<&Value> {
    let Some(arr) = val.get("items").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .map(|entry| entry.get("item").unwrap_or(entry))
        .collect()
}

fn extract_wrapped_tracks(val: &Value) -> Vec<TrackInfo> {
    let Some(arr) = val.get("items").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|entry| {
            if entry.get("type").and_then(|t| t.as_str()) == Some("video") {
                return None;
            }
            let obj = entry.get("item").unwrap_or(entry);
            if obj.get("type").and_then(|t| t.as_str()) == Some("video") {
                return None;
            }
            normalize_track(obj)
        })
        .collect()
}

fn artist_names_and_primary_id(obj: &Value) -> (Vec<String>, Option<u64>) {
    let mut names = Vec::new();
    let mut primary_id = None;
    if let Some(arr) = obj.get("artists").and_then(|v| v.as_array()) {
        for a in arr {
            if primary_id.is_none() {
                primary_id = a.get("id").and_then(|v| v.as_u64());
            }
            if let Some(name) = a.get("name").and_then(|v| v.as_str()) {
                if !name.is_empty() {
                    names.push(name.to_string());
                }
            }
        }
    }
    if names.is_empty() {
        if let Some(a) = obj.get("artist") {
            if primary_id.is_none() {
                primary_id = a.get("id").and_then(|v| v.as_u64());
            }
            if let Some(name) = a.get("name").and_then(|v| v.as_str()) {
                if !name.is_empty() {
                    names.push(name.to_string());
                }
            }
        }
    }
    (names, primary_id)
}

pub fn normalize_track(obj: &Value) -> Option<TrackInfo> {
    let id = obj.get("id").and_then(|v| v.as_u64())?;
    let title = obj.get("title").and_then(|v| v.as_str())?.to_string();
    let (artists, artist_id) = artist_names_and_primary_id(obj);
    let artist_name = if artists.is_empty() {
        "Unknown Artist".to_string()
    } else {
        artists.join(", ")
    };

    let album = obj.get("album");
    let album_name = album
        .and_then(|a| a.get("title"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let album_id = album.and_then(|a| a.get("id")).and_then(|v| v.as_u64());
    let album_cover = album
        .and_then(|a| a.get("cover"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let album_artist = album
        .and_then(|a| a.get("artist"))
        .and_then(|a| a.get("name"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| artist_name.clone());
    let album_copyright = obj
        .get("copyright")
        .or_else(|| album.and_then(|a| a.get("copyright")))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let album_release_year = album
        .and_then(|a| a.get("releaseDate"))
        .and_then(|v| v.as_str())
        .and_then(|d| d.split('-').next()?.parse().ok());

    let hires = obj
        .get("mediaMetadata")
        .and_then(|m| m.get("tags"))
        .and_then(|t| t.as_array())
        .map(|tags| tags.iter().any(|x| x.as_str() == Some("HIRES_LOSSLESS")))
        .unwrap_or(false);

    Some(TrackInfo {
        id,
        title,
        artist_name,
        artist_id,
        artists,
        album_name,
        album_id,
        album_cover,
        album_artist,
        album_copyright,
        album_release_year,
        track_num: obj.get("trackNumber").and_then(|v| v.as_u64()).unwrap_or(1) as u32,
        volume_num: obj.get("volumeNumber").and_then(|v| v.as_u64()).unwrap_or(1) as u32,
        isrc: obj.get("isrc").and_then(|v| v.as_str()).map(|s| s.to_string()),
        audio_quality: obj
            .get("audioQuality")
            .and_then(|v| v.as_str())
            .unwrap_or("HIGH")
            .to_string(),
        duration: obj.get("duration").and_then(|v| v.as_u64()).unwrap_or(0),
        explicit: obj.get("explicit").and_then(|v| v.as_bool()).unwrap_or(false),
        hires,
        local_path: None,
    })
}

pub fn normalize_album(obj: &Value) -> Option<AlbumInfo> {
    let id = obj.get("id").and_then(|v| v.as_u64())?;
    let title = obj.get("title").and_then(|v| v.as_str())?.to_string();
    let (artists, artist_id) = artist_names_and_primary_id(obj);
    let artist_name = if artists.is_empty() {
        "Various Artists".to_string()
    } else {
        artists.join(", ")
    };
    let release_date = obj.get("releaseDate").and_then(|v| v.as_str()).unwrap_or("");
    let release_year = if release_date.len() >= 4 {
        Some(release_date[..4].to_string())
    } else {
        None
    };

    Some(AlbumInfo {
        id,
        title,
        artist_name,
        artist_id,
        cover: obj
            .get("cover")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string()),
        release_year,
        track_count: obj.get("numberOfTracks").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        duration: obj.get("duration").and_then(|v| v.as_u64()).unwrap_or(0),
        audio_quality: obj
            .get("audioQuality")
            .and_then(|v| v.as_str())
            .unwrap_or("HIGH")
            .to_string(),
        explicit: obj.get("explicit").and_then(|v| v.as_bool()).unwrap_or(false),
        copyright: obj
            .get("copyright")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    })
}

pub fn normalize_artist(obj: &Value) -> Option<ArtistInfo> {
    let id = obj.get("id").and_then(|v| v.as_u64())?;
    let name = obj.get("name").and_then(|v| v.as_str())?.to_string();
    let picture = obj
        .get("picture")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    Some(ArtistInfo { id, name, picture })
}

pub fn normalize_playlist(obj: &Value) -> Option<PlaylistInfo> {
    let uuid = obj
        .get("uuid")
        .and_then(|v| v.as_str())
        .or_else(|| obj.get("id").and_then(|v| v.as_str()))
        .filter(|s| !s.is_empty())?
        .to_string();
    let title = obj.get("title").and_then(|v| v.as_str())?.to_string();
    let cover = obj
        .get("squareImage")
        .or_else(|| obj.get("image"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let creator = obj
        .get("creator")
        .and_then(|c| c.get("name"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("Tidal")
        .to_string();

    Some(PlaylistInfo {
        uuid,
        title,
        description: obj
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        creator,
        cover,
        track_count: obj.get("numberOfTracks").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        duration: obj.get("duration").and_then(|v| v.as_u64()).unwrap_or(0),
    })
}

pub fn parse_home_page(val: &Value) -> Vec<HomeSection> {
    let mut sections = Vec::new();
    let Some(rows) = val.get("rows").and_then(|v| v.as_array()) else {
        return sections;
    };
    for row in rows {
        let Some(modules) = row.get("modules").and_then(|v| v.as_array()) else {
            continue;
        };
        for m in modules {
            let mtype = m.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let title = m
                .get("title")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("Recommended")
                .to_string();
            let Some(items) = m
                .get("pagedList")
                .and_then(|p| p.get("items"))
                .and_then(|i| i.as_array())
            else {
                continue;
            };
            match mtype {
                "ALBUM_LIST" => {
                    let list: Vec<AlbumInfo> = items.iter().filter_map(normalize_album).collect();
                    if !list.is_empty() {
                        sections.push(HomeSection {
                            title,
                            items: HomeItems::Albums(list),
                        });
                    }
                }
                "PLAYLIST_LIST" => {
                    let list: Vec<PlaylistInfo> =
                        items.iter().filter_map(normalize_playlist).collect();
                    if !list.is_empty() {
                        sections.push(HomeSection {
                            title,
                            items: HomeItems::Playlists(list),
                        });
                    }
                }
                "TRACK_LIST" => {
                    let list: Vec<TrackInfo> = items.iter().filter_map(normalize_track).collect();
                    if !list.is_empty() {
                        sections.push(HomeSection {
                            title,
                            items: HomeItems::Tracks(list),
                        });
                    }
                }
                "MIX_LIST" => {
                    let list: Vec<MixInfo> = items.iter().filter_map(parse_raw_mix).collect();
                    if !list.is_empty() {
                        sections.push(HomeSection {
                            title,
                            items: HomeItems::Mixes(list),
                        });
                    }
                }
                _ => {}
            }
        }
    }
    sections
}

fn parse_raw_mix(obj: &Value) -> Option<MixInfo> {
    let mix_type = obj.get("mixType").and_then(|v| v.as_str()).unwrap_or("");
    if mix_type.contains("VIDEO") {
        return None;
    }
    let id = obj.get("id").and_then(|v| v.as_str())?.to_string();
    let title = obj.get("title").and_then(|v| v.as_str())?.to_string();
    let subtitle = obj
        .get("subTitle")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let cover = obj
        .get("images")
        .and_then(|imgs| imgs.get("MEDIUM").or_else(|| imgs.get("LARGE")).or_else(|| imgs.get("SMALL")))
        .and_then(|img| img.get("url"))
        .and_then(|u| u.as_str())
        .map(|s| s.to_string());
    Some(MixInfo {
        id,
        title,
        subtitle,
        cover,
    })
}

pub fn parse_mixes_page(val: &Value) -> Vec<MixInfo> {
    let mut mixes = Vec::new();
    let Some(rows) = val.get("rows").and_then(|v| v.as_array()) else {
        return mixes;
    };
    for row in rows {
        let Some(modules) = row.get("modules").and_then(|v| v.as_array()) else {
            continue;
        };
        for m in modules {
            if let Some(items) = m
                .get("pagedList")
                .and_then(|p| p.get("items"))
                .and_then(|i| i.as_array())
            {
                for raw in items {
                    if let Some(mix) = parse_raw_mix(raw) {
                        mixes.push(mix);
                    }
                }
            }
        }
    }
    mixes
}

pub fn parse_search_results(query: &str, val: &Value) -> SearchResults {
    let tracks = val
        .get("tracks")
        .and_then(|t| t.get("items"))
        .and_then(|i| i.as_array())
        .map(|arr| arr.iter().filter_map(normalize_track).collect())
        .unwrap_or_default();
    let albums = val
        .get("albums")
        .and_then(|t| t.get("items"))
        .and_then(|i| i.as_array())
        .map(|arr| arr.iter().filter_map(normalize_album).collect())
        .unwrap_or_default();
    let artists = val
        .get("artists")
        .and_then(|t| t.get("items"))
        .and_then(|i| i.as_array())
        .map(|arr| arr.iter().filter_map(normalize_artist).collect())
        .unwrap_or_default();
    let playlists = val
        .get("playlists")
        .and_then(|t| t.get("items"))
        .and_then(|i| i.as_array())
        .map(|arr| arr.iter().filter_map(normalize_playlist).collect())
        .unwrap_or_default();

    SearchResults {
        query: query.to_string(),
        tracks,
        albums,
        artists,
        playlists,
    }
}

/// Parse DASH MPD XML (supports `<BaseURL>` single-file and `<SegmentTemplate>` + `<SegmentTimeline>`).
pub fn parse_dash_manifest(xml: &str) -> Result<(Vec<String>, String)> {
    let codecs = extract_xml_attr(xml, "Representation", "codecs")
        .unwrap_or_else(|| "mp4a.40.2".to_string());

    if let Some(start) = xml.find("<BaseURL>") {
        if let Some(end) = xml[start..].find("</BaseURL>") {
            let url = xml[start + 9..start + end].trim().to_string();
            if !url.is_empty() {
                return Ok((vec![url], codecs));
            }
        }
    }

    let rep_id = extract_xml_attr(xml, "Representation", "id").unwrap_or_default();
    let bandwidth = extract_xml_attr(xml, "Representation", "bandwidth").unwrap_or_default();
    let media_tmpl = extract_xml_attr(xml, "SegmentTemplate", "media")
        .ok_or_else(|| anyhow!("DASH manifest missing media template and BaseURL"))?;
    let init_tmpl = extract_xml_attr(xml, "SegmentTemplate", "initialization");
    let start_num: u64 = extract_xml_attr(xml, "SegmentTemplate", "startNumber")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
        .max(1);
    let timescale: u64 = extract_xml_attr(xml, "SegmentTemplate", "timescale")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
        .max(1);
    let seg_dur: u64 = extract_xml_attr(xml, "SegmentTemplate", "duration")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let expand = |tmpl: &str, number: u64, time: u64| -> String {
        let mut s = tmpl
            .replace("$RepresentationID$", &rep_id)
            .replace("$Bandwidth$", &bandwidth)
            .replace("$Number$", &number.to_string())
            .replace("$Time$", &time.to_string());
        if let Some(pos) = s.find("$Number%") {
            if let Some(end) = s[pos..].find("d$") {
                let width: usize = s[pos + 8..pos + end].parse().unwrap_or(1);
                let formatted = format!("{:0width$}", number, width = width);
                s = format!("{}{}{}", &s[..pos], formatted, &s[pos + end + 2..]);
            }
        }
        s
    };

    let mut urls = Vec::new();
    if let Some(init) = init_tmpl {
        if !init.is_empty() {
            urls.push(expand(&init, start_num, 0));
        }
    }

    let segments = parse_segment_timeline(xml);
    if !segments.is_empty() {
        let mut number = start_num;
        let mut time = segments[0].0.unwrap_or(0);
        for (t_opt, d, r) in segments {
            if let Some(t) = t_opt {
                time = t;
            }
            let repeat = r + 1;
            for _ in 0..repeat {
                urls.push(expand(&media_tmpl, number, time));
                number += 1;
                time += d;
            }
        }
    } else if seg_dur > 0 {
        let mpd_secs = extract_xml_attr(xml, "MPD", "mediaPresentationDuration")
            .and_then(|d| parse_iso8601_duration(&d))
            .unwrap_or(0.0);
        if mpd_secs > 0.0 {
            let count = ((mpd_secs * timescale as f64) / seg_dur as f64).ceil() as u64;
            for i in 0..count {
                urls.push(expand(&media_tmpl, start_num + i, 0));
            }
        }
    }

    if urls.is_empty() {
        return Err(anyhow!("Tidal DASH manifest yielded no segment URLs"));
    }
    Ok((urls, codecs))
}

fn extract_xml_attr(xml: &str, tag: &str, attr: &str) -> Option<String> {
    let needle = format!("<{}", tag);
    let idx = xml.find(&needle)?;
    let rest = &xml[idx..];
    let tag_end = rest.find('>')?;
    let tag_body = &rest[..tag_end];
    let attr_needle = format!("{}=\"", attr);
    let a_idx = tag_body.find(&attr_needle)?;
    let val_start = &tag_body[a_idx + attr_needle.len()..];
    let val_end = val_start.find('"')?;
    Some(val_start[..val_end].replace("&amp;", "&"))
}

fn parse_segment_timeline(xml: &str) -> Vec<(Option<u64>, u64, u64)> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(pos) = rest.find("<S ") {
        rest = &rest[pos..];
        let Some(end) = rest.find('>') else { break };
        let elem = &rest[..end];
        let t = extract_attr_inline(elem, "t").and_then(|s| s.parse().ok());
        let d = extract_attr_inline(elem, "d")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let r = extract_attr_inline(elem, "r")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if d > 0 {
            out.push((t, d, r));
        }
        rest = &rest[end + 1..];
    }
    out
}

fn extract_attr_inline(tag_body: &str, attr: &str) -> Option<String> {
    let needle = format!("{}=\"", attr);
    let idx = tag_body.find(&needle)?;
    let start = &tag_body[idx + needle.len()..];
    let end = start.find('"')?;
    Some(start[..end].to_string())
}

fn parse_iso8601_duration(s: &str) -> Option<f64> {
    let body = s.strip_prefix("PT")?;
    let mut total = 0.0;
    let mut num = String::new();
    for c in body.chars() {
        if c.is_ascii_digit() || c == '.' {
            num.push(c);
        } else {
            let v: f64 = num.parse().ok()?;
            num.clear();
            match c {
                'H' => total += v * 3600.0,
                'M' => total += v * 60.0,
                'S' => total += v,
                _ => {}
            }
        }
    }
    Some(total)
}

// Publicly known Tidal master key used to unwrap OLD_AES per-track keys.
const TIDAL_MASTER_KEY: &[u8] = &[
    0x50, 0x89, 0x53, 0x4C, 0x43, 0x26, 0x98, 0xB7,
    0xC6, 0xA3, 0x0A, 0x3F, 0x50, 0x2E, 0xB4, 0xC7,
    0x61, 0xF8, 0xE5, 0x6E, 0x8C, 0x74, 0x68, 0x13,
    0x45, 0xFA, 0x3F, 0xBA, 0x68, 0x38, 0xEF, 0x9E,
];

pub fn decrypt_security_token(key_id: &str) -> Result<EncryptionInfo> {
    use cbc::cipher::{block_padding::NoPadding, BlockDecryptMut, KeyIvInit};
    type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;

    let token = base64::engine::general_purpose::STANDARD
        .decode(key_id)
        .map_err(|e| anyhow!("keyId base64 decode failed: {}", e))?;
    if token.len() < 40 {
        return Err(anyhow!("Security token too short ({} bytes)", token.len()));
    }

    let iv = &token[..16];
    let mut buf = token[16..].to_vec();
    let rem = buf.len() % 16;
    if rem != 0 {
        buf.extend(std::iter::repeat(0u8).take(16 - rem));
    }

    Aes256CbcDec::new_from_slices(TIDAL_MASTER_KEY, iv)
        .map_err(|_| anyhow!("Invalid master key/IV length"))?
        .decrypt_padded_mut::<NoPadding>(&mut buf)
        .map_err(|_| anyhow!("AES-CBC decryption failed"))?;

    let mut key = [0u8; 16];
    let mut nonce = [0u8; 8];
    key.copy_from_slice(&buf[..16]);
    nonce.copy_from_slice(&buf[16..24]);
    Ok(EncryptionInfo { key, nonce })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalizes_track_album_artist_and_playlist() {
        let track_json = json!({
            "id": 86430568,
            "title": "Escape",
            "duration": 225,
            "audioQuality": "LOSSLESS",
            "trackNumber": 3,
            "volumeNumber": 1,
            "explicit": false,
            "artists": [{"id": 100, "name": "Netsky"}],
            "album": {"id": 200, "title": "Second Nature", "cover": "ab-cd-ef", "releaseDate": "2020-10-30"},
            "mediaMetadata": {"tags": ["LOSSLESS", "HIRES_LOSSLESS"]}
        });
        let t = normalize_track(&track_json).unwrap();
        assert_eq!(t.id, 86430568);
        assert_eq!(t.artist_name, "Netsky");
        assert_eq!(t.album_release_year, Some(2020));
        assert!(t.hires);

        let pl_json = json!({
            "uuid": "pl-123",
            "title": "Chill",
            "numberOfTracks": 12,
            "duration": 1800,
            "squareImage": "11-22-33",
            "creator": {"name": "Val"}
        });
        let p = normalize_playlist(&pl_json).unwrap();
        assert_eq!(p.uuid, "pl-123");
        assert_eq!(p.creator, "Val");
    }

    #[test]
    fn parses_dash_baseurl_and_segment_template() {
        let baseurl_mpd = r#"<MPD><Period><AdaptationSet><Representation id="1" codecs="flac"><BaseURL>https://cdn.example/track.flac</BaseURL></Representation></AdaptationSet></Period></MPD>"#;
        let (urls, codecs) = parse_dash_manifest(baseurl_mpd).unwrap();
        assert_eq!(urls, vec!["https://cdn.example/track.flac"]);
        assert_eq!(codecs, "flac");

        let seg_mpd = r#"<MPD mediaPresentationDuration="PT1M"><Period><AdaptationSet><Representation id="0" codecs="flac" bandwidth="900000"><SegmentTemplate initialization="https://cdn.example/init.mp4" media="https://cdn.example/seg_$Number$.m4s" startNumber="1" timescale="44100"><SegmentTimeline><S t="0" d="88200" r="2"/></SegmentTimeline></SegmentTemplate></Representation></AdaptationSet></Period></MPD>"#;
        let (seg_urls, _) = parse_dash_manifest(seg_mpd).unwrap();
        assert_eq!(seg_urls.len(), 4);
        assert_eq!(seg_urls[0], "https://cdn.example/init.mp4");
        assert_eq!(seg_urls[3], "https://cdn.example/seg_3.m4s");
    }
}
