use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const CLIENT_ID: &str = "fX2JxdmntZWK0ixT";
pub const CLIENT_SECRET: &str = "1Nn9AfDAjxrgJFJbKNWLeAyKGVGmINuXPPLHVXAvxAg=";
const AUTH_BASE: &str = "https://auth.tidal.com/v1/oauth2";

pub const TIDAL_UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
    (KHTML, like Gecko) TIDAL/2.41.3 Chrome/142.0.7444.265 Electron/39.8.0 Safari/537.36";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    #[serde(alias = "expiry")]
    pub expiry_time: String,
    #[serde(default)]
    pub user_id: u64,
    #[serde(default = "default_country")]
    pub country_code: String,
    #[serde(default)]
    pub user_name: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default = "default_token_type")]
    pub token_type: String,
}

fn default_token_type() -> String {
    "Bearer".to_string()
}

fn default_country() -> String {
    "US".to_string()
}

impl Session {
    pub fn is_expired(&self) -> bool {
        if self.access_token.is_empty() {
            return true;
        }
        if let Ok(t) = DateTime::parse_from_rfc3339(&self.expiry_time) {
            let expiry: DateTime<Utc> = t.into();
            return Utc::now() >= expiry - chrono::Duration::seconds(60);
        }
        true
    }

    pub fn auth_header(&self) -> String {
        let tt = if self.token_type.is_empty() {
            "Bearer"
        } else {
            &self.token_type
        };
        format!("{} {}", tt, self.access_token)
    }

    pub fn display_label(&self) -> String {
        if let Some(ref name) = self.user_name {
            if !name.is_empty() {
                return format!("{} [{}]", name, self.country_code);
            }
        }
        if self.user_id > 0 {
            format!("{} [{}]", self.user_id, self.country_code)
        } else {
            format!("Tidal [{}]", self.country_code)
        }
    }
}

pub fn session_path() -> PathBuf {
    crate::config::config_dir().join("session.json")
}

pub fn import_candidate_paths() -> Vec<PathBuf> {
    let config_root = dirs::config_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config"));
    let home = dirs::home_dir().unwrap_or_default();

    vec![
        session_path(),
        home.join(".config").join("lumitide").join("session.json"),
        config_root.join("lumitide").join("session.json"),
        home.join(".config").join("tide").join("session.json"),
        config_root.join("tide").join("session.json"),
        home.join(".config").join("undertow").join("undertow").join("session.json"),
        config_root.join("undertow").join("undertow").join("session.json"),
        home.join(".config").join("undertow").join("session.json"),
    ]
}

pub fn load_session_from_path(path: &Path) -> Option<Session> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut s: Session = serde_json::from_str(&text).ok()?;
    if s.access_token.is_empty() && s.refresh_token.is_empty() {
        return None;
    }
    if s.country_code.is_empty() {
        s.country_code = "US".to_string();
    }
    if s.token_type.is_empty() {
        s.token_type = "Bearer".to_string();
    }
    Some(s)
}

/// Try loading an existing session from `tidal-tui` or importing from Lumitide/Tide/Undertow.
/// Refreshes automatically if expired. Does NOT trigger interactive login.
pub fn load_or_import_session() -> Option<Session> {
    let own = session_path();
    for path in import_candidate_paths() {
        if !path.exists() {
            continue;
        }
        if let Some(session) = load_session_from_path(&path) {
            if !session.is_expired() {
                if path != own {
                    let _ = save_session(&session);
                }
                return Some(session);
            }
            if !session.refresh_token.is_empty() {
                if let Ok(mut refreshed) = refresh_token(&session.refresh_token) {
                    if refreshed.user_id == 0 {
                        refreshed.user_id = session.user_id;
                    }
                    if refreshed.country_code.is_empty() {
                        refreshed.country_code = session.country_code.clone();
                    }
                    if refreshed.user_name.is_none() {
                        refreshed.user_name = session.user_name.clone();
                    }
                    let _ = save_session(&refreshed);
                    return Some(refreshed);
                }
            }
        }
    }
    None
}

pub fn save_session(session: &Session) -> Result<()> {
    let path = session_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(session)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

pub fn clear_session() -> Result<()> {
    let path = session_path();
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

#[derive(Deserialize)]
struct TokenResp {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
    #[serde(default = "default_token_type")]
    token_type: String,
    user: Option<UserResp>,
}

#[derive(Deserialize)]
struct UserResp {
    #[serde(rename = "userId")]
    user_id: u64,
    #[serde(rename = "countryCode")]
    country_code: String,
    username: Option<String>,
}

fn parse_token_response(data: TokenResp, fallback_refresh: &str) -> Session {
    let expiry = Utc::now() + chrono::Duration::seconds(data.expires_in as i64);
    let user_name = data
        .user
        .as_ref()
        .and_then(|u| u.username.clone())
        .filter(|n| !n.is_empty());
    Session {
        access_token: data.access_token,
        refresh_token: data
            .refresh_token
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| fallback_refresh.to_string()),
        expiry_time: expiry.to_rfc3339(),
        user_id: data.user.as_ref().map(|u| u.user_id).unwrap_or(0),
        country_code: data
            .user
            .map(|u| u.country_code)
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| "US".to_string()),
        user_name,
        session_id: None,
        token_type: data.token_type,
    }
}

pub fn refresh_token(refresh_token: &str) -> Result<Session> {
    if refresh_token.is_empty() {
        return Err(anyhow!("Missing refresh token"));
    }
    let client = reqwest::blocking::Client::builder()
        .user_agent(TIDAL_UA)
        .timeout(std::time::Duration::from_secs(20))
        .build()?;

    let resp = client
        .post(format!("{}/token", AUTH_BASE))
        .form(&[
            ("client_id", CLIENT_ID),
            ("client_secret", CLIENT_SECRET),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("scope", "r_usr w_usr w_sub"),
        ])
        .send()?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        return Err(anyhow!("Token refresh failed ({}): {}", status, body));
    }

    let data: TokenResp = resp.json()?;
    Ok(parse_token_response(data, refresh_token))
}

#[derive(Debug, Clone)]
pub struct DeviceAuthState {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Debug, Clone)]
pub enum DevicePollResult {
    Pending,
    SlowDown,
    Authorized(Session),
    Denied,
    Expired,
}

pub fn start_device_auth() -> Result<DeviceAuthState> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(TIDAL_UA)
        .timeout(std::time::Duration::from_secs(20))
        .build()?;

    #[derive(Deserialize)]
    struct DeviceResp {
        #[serde(rename = "deviceCode")]
        device_code: String,
        #[serde(rename = "userCode")]
        user_code: String,
        #[serde(rename = "verificationUriComplete")]
        verification_uri_complete: Option<String>,
        #[serde(rename = "verificationUri")]
        verification_uri: String,
        #[serde(rename = "expiresIn")]
        expires_in: u64,
        interval: u64,
    }

    let device: DeviceResp = client
        .post(format!("{}/device_authorization", AUTH_BASE))
        .form(&[("client_id", CLIENT_ID), ("scope", "r_usr w_usr w_sub")])
        .send()?
        .error_for_status()?
        .json()?;

    let mut uri = device
        .verification_uri_complete
        .filter(|s| !s.is_empty())
        .unwrap_or(device.verification_uri);
    if !uri.starts_with("http") {
        uri = format!("https://{}", uri);
    }

    Ok(DeviceAuthState {
        device_code: device.device_code,
        user_code: device.user_code,
        verification_uri: uri,
        expires_in: device.expires_in,
        interval: device.interval.max(2),
    })
}

pub fn poll_device_auth_once(state: &DeviceAuthState) -> Result<DevicePollResult> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(TIDAL_UA)
        .timeout(std::time::Duration::from_secs(20))
        .build()?;

    let resp = client
        .post(format!("{}/token", AUTH_BASE))
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", &state.device_code),
            ("client_id", CLIENT_ID),
            ("client_secret", CLIENT_SECRET),
            ("scope", "r_usr w_usr w_sub"),
        ])
        .send()?;

    if resp.status().is_success() {
        let data: TokenResp = resp.json()?;
        let session = parse_token_response(data, "");
        let _ = save_session(&session);
        return Ok(DevicePollResult::Authorized(session));
    }

    let status = resp.status();
    let body = resp.text().unwrap_or_default();
    if let Ok(err_json) = serde_json::from_str::<serde_json::Value>(&body) {
        if let Some(err) = err_json.get("error").and_then(|v| v.as_str()) {
            match err {
                "authorization_pending" => return Ok(DevicePollResult::Pending),
                "slow_down" => return Ok(DevicePollResult::SlowDown),
                "expired_token" => return Ok(DevicePollResult::Expired),
                "access_denied" => return Ok(DevicePollResult::Denied),
                _ => {
                    let desc = err_json
                        .get("error_description")
                        .and_then(|v| v.as_str())
                        .unwrap_or(err);
                    return Err(anyhow!("{}", desc));
                }
            }
        }
    }

    Err(anyhow!("Auth poll failed ({}): {}", status, body))
}

pub fn open_browser(url: &str) {
    use std::process::{Command, Stdio};

    #[cfg(target_os = "linux")]
    let _ = Command::new("xdg-open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    #[cfg(target_os = "macos")]
    let _ = Command::new("open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    #[cfg(target_os = "windows")]
    let _ = Command::new("powershell")
        .args(["-NoProfile", "-Command", &format!("Start-Process '{}'", url.replace('\'', "''"))])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lumitide_session_json() {
        let raw = r#"{
            "access_token": "abc",
            "refresh_token": "def",
            "expiry_time": "2099-01-01T00:00:00Z",
            "user_id": 209654258,
            "country_code": "MY",
            "token_type": "Bearer"
        }"#;
        let s: Session = serde_json::from_str(raw).unwrap();
        assert_eq!(s.user_id, 209654258);
        assert_eq!(s.country_code, "MY");
        assert!(!s.is_expired());
    }

    #[test]
    fn parses_undertow_session_json_with_expiry_alias() {
        let raw = r#"{
            "access_token": "xyz",
            "refresh_token": "ref",
            "expiry": "2099-06-01T12:00:00Z",
            "user_id": 42,
            "country_code": "US",
            "user_name": "Listener",
            "session_id": "sess-1"
        }"#;
        let s: Session = serde_json::from_str(raw).unwrap();
        assert_eq!(s.user_id, 42);
        assert_eq!(s.user_name.as_deref(), Some("Listener"));
        assert!(!s.is_expired());
    }
}
