use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ── Platform-specific client credentials ─────────────────────────────────────
//
// Windows: Tidal desktop app client — PKCE flow, no secret, issues INTERNAL
//          tokens that unlock LOSSLESS FLAC streams.
// Other:   Legacy third-party client — device code flow, issues BROWSER tokens
//          that give HIGH quality (MP4/AAC). Good UX, works on all platforms.

#[cfg(target_os = "windows")]
pub const CLIENT_ID: &str = "mhPVJJEBNRzVjr2p";

#[cfg(not(target_os = "windows"))]
pub const CLIENT_ID: &str = "fX2JxdmntZWK0ixT";
#[cfg(not(target_os = "windows"))]
const CLIENT_SECRET: &str = "1Nn9AfDAjxrgJFJbKNWLeAyKGVGmINuXPPLHVXAvxAg=";

const AUTH_BASE: &str = "https://auth.tidal.com/v1/oauth2";

// Desktop UA — required for the Windows PKCE flow and keeps API requests consistent.
pub const TIDAL_UA: &str = "Mozilla/5.0 (Windows NT 10.0; WOW64) AppleWebKit/537.36 \
    (KHTML, like Gecko) TIDAL/2.41.3 Chrome/142.0.7444.265 Electron/39.8.0 Safari/537.36";

fn session_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".config")
        .join("lumitide")
        .join("session.json")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub access_token: String,
    pub refresh_token: String,
    pub expiry_time: String, // ISO 8601
    pub user_id: u64,
    pub country_code: String,
    #[serde(default = "default_token_type")]
    pub token_type: String,
}

fn default_token_type() -> String { "Bearer".to_string() }

impl Session {
    pub fn is_expired(&self) -> bool {
        if let Ok(t) = DateTime::parse_from_rfc3339(&self.expiry_time) {
            let expiry: DateTime<Utc> = t.into();
            return Utc::now() >= expiry - chrono::Duration::seconds(60);
        }
        true
    }

    pub fn auth_header(&self) -> String {
        format!("Bearer {}", self.access_token)
    }
}

/// Load session from disk, refresh if expired, or run the platform login flow.
pub fn get_session() -> Result<Session> {
    let path = session_path();
    if path.exists() {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(mut session) = serde_json::from_str::<Session>(&text) {
                if !session.is_expired() {
                    return Ok(session);
                }
                if let Ok(refreshed) = refresh_token(&session.refresh_token) {
                    session = refreshed;
                    let _ = save_session(&session);
                    return Ok(session);
                }
            }
        }
    }

    #[cfg(target_os = "windows")]
    let session = pkce_login()?;
    #[cfg(not(target_os = "windows"))]
    let session = device_auth_flow()?;

    let _ = save_session(&session);
    Ok(session)
}

pub fn save_session(session: &Session) -> Result<()> {
    let path = session_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(session)?)?;
    Ok(())
}

pub fn refresh_token(refresh_token: &str) -> Result<Session> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(TIDAL_UA)
        .build()?;

    #[cfg(target_os = "windows")]
    let form: &[(&str, &str)] = &[
        ("client_id",      CLIENT_ID),
        ("grant_type",     "refresh_token"),
        ("refresh_token",  refresh_token),
        ("scope",          "r_usr w_usr"),
    ];

    #[cfg(not(target_os = "windows"))]
    let form: &[(&str, &str)] = &[
        ("client_id",      CLIENT_ID),
        ("client_secret",  CLIENT_SECRET),
        ("grant_type",     "refresh_token"),
        ("refresh_token",  refresh_token),
        ("scope",          "r_usr w_usr"),
    ];

    let resp = client
        .post(format!("{}/token", AUTH_BASE))
        .form(form)
        .send()?;

    if !resp.status().is_success() {
        return Err(anyhow!("Token refresh failed: {}", resp.status()));
    }

    parse_token_response(resp.json()?, refresh_token)
}

// ─── Windows: PKCE login ──────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
const TIDAL_REDIRECT: &str = "tidal://login/auth";

/// Path of the temp file the callback process writes the tidal:// URL into.
#[cfg(target_os = "windows")]
pub fn auth_callback_file() -> std::path::PathBuf {
    std::env::temp_dir().join("lumitide_auth.tmp")
}

#[cfg(target_os = "windows")]
fn pkce_login() -> Result<Session> {
    let (verifier, challenge) = pkce_pair();
    let cuk = new_uuid();

    let auth_url = format!(
        "{}/authorize?client_id={}&client_unique_key={}&code_challenge={}\
         &code_challenge_method=S256&redirect_uri={}&response_type=code&scope=r_usr+w_usr",
        "https://login.tidal.com", CLIENT_ID, cuk, challenge,
        percent_encode(TIDAL_REDIRECT),
    );

    pkce_login_windows(auth_url, verifier, cuk)
}

/// Temporarily register lumitide as the tidal:// handler so the OS calls us back
/// automatically after the user logs in, then restore the original handler.
#[cfg(target_os = "windows")]
fn pkce_login_windows(auth_url: String, verifier: String, cuk: String) -> Result<Session> {
    let exe = std::env::current_exe()
        .map_err(|e| anyhow!("Could not determine exe path: {}", e))?;
    let exe_str = exe.to_string_lossy();
    let handler_cmd = format!("\"{}\" --auth-callback \"%1\"", exe_str);

    let callback_file = auth_callback_file();
    let _ = std::fs::remove_file(&callback_file);

    let reg_base = r"HKCU\Software\Classes\tidal";
    let _ = std::process::Command::new("reg")
        .args(["add", reg_base, "/ve", "/t", "REG_SZ", "/d", "URL:tidal Protocol", "/f"])
        .output();
    let _ = std::process::Command::new("reg")
        .args(["add", reg_base, "/v", "URL Protocol", "/t", "REG_SZ", "/d", "", "/f"])
        .output();
    let _ = std::process::Command::new("reg")
        .args(["add", &format!(r"{}\shell\open\command", reg_base),
               "/ve", "/t", "REG_SZ", "/d", &handler_cmd, "/f"])
        .output();

    open_browser(&auth_url);
    println!("\nOpening Tidal login in your browser...");
    println!("If it doesn't open automatically, visit:");
    println!("  {}\n", auth_url);
    println!("Log in and approve — Lumitide will handle the redirect automatically.");
    println!("Waiting for Tidal callback...\n");

    let code = poll_callback_file(&callback_file, std::time::Duration::from_secs(180));

    // Always restore — delete HKCU override so HKLM (Tidal app) takes over again.
    let _ = std::process::Command::new("reg")
        .args(["delete", reg_base, "/f"])
        .output();

    exchange_code(&code?, &verifier, &cuk, TIDAL_REDIRECT)
}

#[cfg(target_os = "windows")]
fn poll_callback_file(path: &std::path::Path, timeout: std::time::Duration) -> Result<String> {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if path.exists() {
            let url = std::fs::read_to_string(path)?.trim().to_string();
            let _ = std::fs::remove_file(path);
            return extract_code_from_url(&url);
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    Err(anyhow!("Timed out waiting for Tidal auth callback"))
}

#[cfg(target_os = "windows")]
fn exchange_code(code: &str, verifier: &str, cuk: &str, redirect_uri: &str) -> Result<Session> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(TIDAL_UA)
        .build()?;
    let resp = client
        .post(format!("{}/token", AUTH_BASE))
        .form(&[
            ("client_id",         CLIENT_ID),
            ("client_unique_key", cuk),
            ("code",              code),
            ("code_verifier",     verifier),
            ("grant_type",        "authorization_code"),
            ("redirect_uri",      redirect_uri),
            ("scope",             "r_usr w_usr"),
        ])
        .send()?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        return Err(anyhow!("Token exchange failed {}: {}", status, body));
    }

    println!("Login successful!");
    parse_token_response(resp.json()?, "")
}

// ─── Non-Windows: device code login ──────────────────────────────────────────

#[cfg(not(target_os = "windows"))]
fn device_auth_flow() -> Result<Session> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(TIDAL_UA)
        .build()?;

    #[derive(Deserialize)]
    struct DeviceResp {
        #[serde(rename = "deviceCode")]
        device_code: String,
        #[serde(rename = "userCode")]
        user_code: String,
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

    println!("\nTo log in to Tidal, visit:");
    println!("  {}", device.verification_uri);
    println!("And enter code: {}", device.user_code);
    println!("\nWaiting for authorisation...");

    let interval = std::time::Duration::from_secs(device.interval.max(5));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(device.expires_in);

    loop {
        if std::time::Instant::now() > deadline {
            return Err(anyhow!("Device authorisation timed out"));
        }
        std::thread::sleep(interval);

        let poll = client
            .post(format!("{}/token", AUTH_BASE))
            .form(&[
                ("grant_type",    "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code",   &device.device_code),
                ("client_id",     CLIENT_ID),
                ("client_secret", CLIENT_SECRET),
                ("scope",         "r_usr w_usr w_sub"),
            ])
            .send()?;

        if poll.status().is_success() {
            #[derive(Deserialize)]
            struct TokenResp {
                access_token: String,
                refresh_token: String,
                expires_in: u64,
                user: UserResp,
            }
            #[derive(Deserialize)]
            struct UserResp {
                #[serde(rename = "userId")]  user_id: u64,
                #[serde(rename = "countryCode")] country_code: String,
            }

            let data: TokenResp = poll.json()?;
            let expiry = Utc::now() + chrono::Duration::seconds(data.expires_in as i64);
            println!("Login successful!");
            return Ok(Session {
                access_token:  data.access_token,
                refresh_token: data.refresh_token,
                expiry_time:   expiry.to_rfc3339(),
                user_id:       data.user.user_id,
                country_code:  data.user.country_code,
                token_type:    "Bearer".to_string(),
            });
        }

        let status = poll.status();
        if !status.is_client_error() || status.as_u16() != 400 {
            return Err(anyhow!("Auth poll failed: {}", status));
        }
    }
}

// ─── Shared helpers ───────────────────────────────────────────────────────────

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
    #[serde(rename = "userId")]      user_id: u64,
    #[serde(rename = "countryCode")] country_code: String,
}

fn parse_token_response(data: TokenResp, fallback_refresh: &str) -> Result<Session> {
    let expiry = Utc::now() + chrono::Duration::seconds(data.expires_in as i64);
    Ok(Session {
        access_token:  data.access_token,
        refresh_token: data.refresh_token.filter(|s| !s.is_empty())
            .unwrap_or_else(|| fallback_refresh.to_string()),
        expiry_time:   expiry.to_rfc3339(),
        user_id:       data.user.as_ref().map(|u| u.user_id).unwrap_or(0),
        country_code:  data.user.map(|u| u.country_code).unwrap_or_default(),
        token_type:    data.token_type,
    })
}

pub fn new_uuid() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    b[6] = (b[6] & 0x0F) | 0x40;
    b[8] = (b[8] & 0x3F) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-\
         {:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0],b[1],b[2],b[3], b[4],b[5], b[6],b[7],
        b[8],b[9], b[10],b[11],b[12],b[13],b[14],b[15]
    )
}

#[cfg(target_os = "windows")]
fn pkce_pair() -> (String, String) {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let hash = Sha256::digest(verifier.as_bytes());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash);
    (verifier, challenge)
}

#[cfg(target_os = "windows")]
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9'
            | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            b => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

#[cfg(target_os = "windows")]
fn extract_code_from_url(url: &str) -> Result<String> {
    let query = url.splitn(2, '?').nth(1).unwrap_or(url);
    query.split('&')
        .find(|p| p.starts_with("code="))
        .and_then(|kv| kv.splitn(2, '=').nth(1))
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("Could not find 'code' parameter in URL: {}", url))
}

fn open_browser(url: &str) {
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &format!("Start-Process '{}'", url.replace('\'', "''"))])
        .spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}
