use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// These are the public app credentials used by tidalapi and other open-source
// Tidal clients (https://github.com/tamland/python-tidal). They are not personal
// credentials — they identify a generic third-party app identity that Tidal
// has historically permitted for non-commercial use.
pub const CLIENT_ID: &str = "fX2JxdmntZWK0ixT";
const CLIENT_SECRET: &str = "1Nn9AfDAjxrgJFJbKNWLeAyKGVGmINuXPPLHVXAvxAg=";
const AUTH_BASE: &str = "https://auth.tidal.com/v1/oauth2";

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
            // Treat as expired 60 seconds early to avoid racing the deadline
            return Utc::now() >= expiry - chrono::Duration::seconds(60);
        }
        true
    }

    pub fn auth_header(&self) -> String {
        format!("Bearer {}", self.access_token)
    }
}

/// Load session from disk, refresh if expired, or run device-auth for a new login.
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

    // No valid session — run the device-auth flow
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
    let client = reqwest::blocking::Client::new();
    let resp = client
        .post(format!("{}/token", AUTH_BASE))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!(
            "grant_type=refresh_token&refresh_token={}&client_id={}&client_secret={}",
            refresh_token, CLIENT_ID, CLIENT_SECRET
        ))
        .send()?;

    if !resp.status().is_success() {
        return Err(anyhow!("Token refresh failed: {}", resp.status()));
    }

    #[derive(Deserialize)]
    struct TokenResp {
        access_token: String,
        refresh_token: Option<String>,
        expires_in: u64,
        #[serde(default = "default_bearer")]
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
    fn default_bearer() -> String { "Bearer".to_string() }

    let data: TokenResp = resp.json()?;
    let expiry = Utc::now() + chrono::Duration::seconds(data.expires_in as i64);
    Ok(Session {
        access_token:  data.access_token,
        refresh_token: data.refresh_token.unwrap_or_else(|| refresh_token.to_string()),
        expiry_time:   expiry.to_rfc3339(),
        user_id:       data.user.as_ref().map(|u| u.user_id).unwrap_or(0),
        country_code:  data.user.map(|u| u.country_code).unwrap_or_default(),
        token_type:    data.token_type,
    })
}

fn device_auth_flow() -> Result<Session> {
    let client = reqwest::blocking::Client::new();

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

    let resp = client
        .post(format!("{}/device_authorization", AUTH_BASE))
        .form(&[("client_id", CLIENT_ID), ("scope", "r_usr w_usr w_sub")])
        .send()?
        .error_for_status()?;

    let device: DeviceResp = resp.json()?;

    println!("\nTo log in to Tidal, visit:");
    println!("  \x1B]8;;{}\x1B\\{}\x1B]8;;\x1B\\", device.verification_uri, device.verification_uri);
    println!("And enter code: {}", device.user_code);
    println!("\nWaiting for authorisation...");

    let interval = std::time::Duration::from_secs(device.interval.max(5));
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(device.expires_in);

    loop {
        if std::time::Instant::now() > deadline {
            return Err(anyhow!("Device authorisation timed out"));
        }
        std::thread::sleep(interval);

        let poll = client
            .post(format!("{}/token", AUTH_BASE))
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", &device.device_code),
                ("client_id", CLIENT_ID),
                ("client_secret", CLIENT_SECRET),
                ("scope", "r_usr w_usr w_sub"),
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
                #[serde(rename = "userId")]
                user_id: u64,
                #[serde(rename = "countryCode")]
                country_code: String,
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
