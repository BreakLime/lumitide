use anyhow::{anyhow, Result};
use base64::Engine;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

// Desktop app client — issues INTERNAL tokens with LOSSLESS access.
pub const CLIENT_ID: &str = "mhPVJJEBNRzVjr2p";
const AUTH_BASE: &str = "https://auth.tidal.com/v1/oauth2";
const LOGIN_BASE: &str = "https://login.tidal.com";
// Match the desktop app User-Agent so auth and playback endpoints accept our requests.
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

/// Load session from disk, refresh if expired, or run PKCE login for a new session.
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

    let session = pkce_login()?;
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
    let resp = client
        .post(format!("{}/token", AUTH_BASE))
        .form(&[
            ("client_id", CLIENT_ID),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("scope", "r_usr w_usr"),
        ])
        .send()?;

    if !resp.status().is_success() {
        return Err(anyhow!("Token refresh failed: {}", resp.status()));
    }

    parse_token_response(resp.json()?, refresh_token)
}

// ─── PKCE login ───────────────────────────────────────────────────────────────

const TIDAL_REDIRECT: &str = "tidal://login/auth";

/// Path of the temp file the callback process writes the tidal:// URL into.
pub fn auth_callback_file() -> std::path::PathBuf {
    std::env::temp_dir().join("lumitide_auth.tmp")
}

fn pkce_login() -> Result<Session> {
    let (verifier, challenge) = pkce_pair();
    let cuk = new_uuid();

    let auth_url = format!(
        "{}/authorize?client_id={}&client_unique_key={}&code_challenge={}\
         &code_challenge_method=S256&redirect_uri={}&response_type=code&scope=r_usr+w_usr",
        LOGIN_BASE, CLIENT_ID, cuk, challenge,
        percent_encode(TIDAL_REDIRECT),
    );

    #[cfg(target_os = "windows")]
    return pkce_login_windows(auth_url, verifier, cuk);

    #[cfg(not(target_os = "windows"))]
    pkce_login_manual(auth_url, verifier, cuk)
}

/// Windows: temporarily register lumitide as the tidal:// handler so the OS
/// calls us back automatically after the user logs in.
#[cfg(target_os = "windows")]
fn pkce_login_windows(auth_url: String, verifier: String, cuk: String) -> Result<Session> {
    let exe = std::env::current_exe()
        .map_err(|e| anyhow!("Could not determine exe path: {}", e))?;
    let exe_str = exe.to_string_lossy();
    let handler_cmd = format!("\"{}\" --auth-callback \"%1\"", exe_str);

    let callback_file = auth_callback_file();
    let _ = std::fs::remove_file(&callback_file);

    // Register HKCU override — takes precedence over HKLM without admin rights.
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

    // Always restore: delete our HKCU override so HKLM (Tidal app) takes over again.
    let _ = std::process::Command::new("reg")
        .args(["delete", reg_base, "/f"])
        .output();

    exchange_code(&code?, &verifier, &cuk, TIDAL_REDIRECT)
}

/// Non-Windows fallback: ask the user to paste the tidal:// URL manually.
#[cfg(not(target_os = "windows"))]
fn pkce_login_manual(auth_url: String, verifier: String, cuk: String) -> Result<Session> {
    open_browser(&auth_url);
    println!("\nOpening Tidal login in your browser...");
    println!("If it doesn't open automatically, visit:");
    println!("  {}\n", auth_url);
    println!("After logging in, copy the full URL your browser tries to open");
    println!("(starts with 'tidal://login/auth?code=...') and paste it below.\n");

    let input: String = dialoguer::Input::new()
        .with_prompt("Paste URL")
        .interact_text()?;
    let code = extract_code_from_url(&input)?;
    exchange_code(&code, &verifier, &cuk, TIDAL_REDIRECT)
}

fn poll_callback_file(
    path: &std::path::Path,
    timeout: std::time::Duration,
) -> Result<String> {
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

fn exchange_code(code: &str, verifier: &str, cuk: &str, redirect_uri: &str) -> Result<Session> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(TIDAL_UA)
        .build()?;
    let resp = client
        .post(format!("{}/token", AUTH_BASE))
        .form(&[
            ("client_id", CLIENT_ID),
            ("client_unique_key", cuk),
            ("code", code),
            ("code_verifier", verifier),
            ("grant_type", "authorization_code"),
            ("redirect_uri", redirect_uri),
            ("scope", "r_usr w_usr"),
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

// ─── Helpers ─────────────────────────────────────────────────────────────────

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

fn pkce_pair() -> (String, String) {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let hash = Sha256::digest(verifier.as_bytes());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash);
    (verifier, challenge)
}

pub fn new_uuid() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    // Set version 4 and variant bits
    b[6] = (b[6] & 0x0F) | 0x40;
    b[8] = (b[8] & 0x3F) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-\
         {:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0],b[1],b[2],b[3], b[4],b[5], b[6],b[7],
        b[8],b[9], b[10],b[11],b[12],b[13],b[14],b[15]
    )
}

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
