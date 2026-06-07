use anyhow::{anyhow, Result};
use base64::Engine;
use serde::Deserialize;

use crate::auth::Session;

const API_BASE: &str = "https://api.tidal.com/v1";

// ─── Public data types ────────────────────────────────────────────────────────

/// Encryption parameters for an AES-128-CTR encrypted track.
#[derive(Clone)]
pub struct EncryptionInfo {
    pub key: [u8; 16],
    pub nonce: [u8; 8],
}

/// Result of a stream_url call: how Tidal is delivering this track's audio.
pub enum StreamInfo {
    /// A single CDN URL (Tidal "bts" manifest), optionally AES-128-CTR encrypted.
    /// Covers Windows lossless FLAC and the MP4/AAC fallback on all platforms.
    Single {
        url: String,
        encryption: Option<EncryptionInfo>,
    },
    /// A multi-segment DASH stream (HiRes FLAC): the init segment followed by the
    /// media segments, in order. Concatenate and remux to native FLAC — these are
    /// unencrypted, so no decryption is needed.
    Dash { segments: Vec<String> },
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // audio_quality and duration stored for metadata embedding / future display
pub struct TrackInfo {
    pub id: u64,
    pub title: String,
    pub artist_name: String,
    pub album_name: String,
    pub album_cover: Option<String>, // UUID like "ab-cd-ef-gh"
    pub album_artist: String,
    pub album_copyright: Option<String>,
    pub album_release_year: Option<i32>,
    pub artists: Vec<String>,
    pub track_num: u32,
    pub volume_num: u32,
    pub isrc: Option<String>,
    pub audio_quality: String, // e.g. "LOSSLESS", "FLAC", "MP3"
    pub duration: u64,         // seconds; stored for future display
}

#[derive(Debug, Clone)]
pub struct ArtistInfo {
    pub id: u64,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct MixInfo {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone)]
pub struct PlaylistInfo {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // cover stored for future album art display
pub struct AlbumInfo {
    pub id: u64,
    pub title: String,
    pub artist_name: String,
    pub cover: Option<String>,
}

// ─── Raw API deserialization ───────────────────────────────────────────────────

#[derive(Deserialize)]
struct RawArtist {
    id: u64,
    name: String,
}

#[derive(Deserialize)]
struct RawAlbum {
    title: String,
    cover: Option<String>,
    artist: Option<RawArtist>,
    copyright: Option<String>,
    #[serde(rename = "releaseDate")]
    release_date: Option<String>,
}

#[derive(Deserialize)]
struct RawTrack {
    id: u64,
    title: String,
    duration: Option<u64>,
    #[serde(rename = "trackNumber")]
    track_number: Option<u32>,
    #[serde(rename = "volumeNumber")]
    volume_number: Option<u32>,
    isrc: Option<String>,
    #[serde(rename = "audioQuality")]
    audio_quality: Option<String>,
    // Some endpoints omit "artist" and only include "artists"
    artist: Option<RawArtist>,
    artists: Option<Vec<RawArtist>>,
    album: RawAlbum,
    copyright: Option<String>,
}

impl From<RawTrack> for TrackInfo {
    fn from(r: RawTrack) -> Self {
        let year = r.album.release_date.as_deref().and_then(|d| {
            d.split('-').next().and_then(|y| y.parse().ok())
        });
        // Prefer the single artist field; fall back to first element of artists array
        let primary_artist = r.artist
            .as_ref()
            .map(|a| a.name.clone())
            .or_else(|| r.artists.as_ref().and_then(|v| v.first()).map(|a| a.name.clone()))
            .unwrap_or_default();
        let copyright = r.copyright.or(r.album.copyright);
        TrackInfo {
            id:                  r.id,
            title:               r.title,
            artist_name:         primary_artist.clone(),
            album_name:          r.album.title,
            album_cover:         r.album.cover,
            album_artist:        r.album.artist.map(|a| a.name).unwrap_or(primary_artist),
            album_copyright:     copyright,
            album_release_year:  year,
            artists:             r.artists.unwrap_or_default().into_iter().map(|a| a.name).collect(),
            track_num:           r.track_number.unwrap_or(1),
            volume_num:          r.volume_number.unwrap_or(1),
            isrc:                r.isrc,
            audio_quality:       r.audio_quality.unwrap_or_else(|| "UNKNOWN".to_string()),
            duration:            r.duration.unwrap_or(0),
        }
    }
}

// ─── Client ───────────────────────────────────────────────────────────────────

pub struct TidalClient {
    pub session: Session,
    client: reqwest::blocking::Client,
}

impl TidalClient {
    pub fn new(session: Session) -> Self {
        let client = reqwest::blocking::Client::builder()
            .user_agent(crate::auth::TIDAL_UA)
            .build()
            .unwrap_or_default();
        Self { session, client }
    }

    fn get(&self, path: &str, params: &[(&str, &str)]) -> Result<reqwest::blocking::Response> {
        let url = format!("{}/{}", API_BASE, path);
        let mut req = self.client.get(&url)
            .header("Authorization", self.session.auth_header());
        for &(k, v) in params {
            req = req.query(&[(k, v)]);
        }
        let resp = req.send()?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            return Err(anyhow!("API error {} on {}: {}", status, path, body));
        }
        Ok(resp)
    }

    // ── Track ────────────────────────────────────────────────────────────────

    pub fn track(&self, id: u64) -> Result<TrackInfo> {
        let resp = self.get(
            &format!("tracks/{}", id),
            &[("countryCode", &self.session.country_code)],
        )?;
        let raw: RawTrack = resp.json()?;
        Ok(raw.into())
    }

    pub fn stream_url(&self, id: u64) -> Result<StreamInfo> {
        #[cfg(target_os = "windows")]
        let quality = "LOSSLESS";
        // Linux/macOS: request HI_RES_LOSSLESS only when the user wants FLAC AND
        // ffmpeg is available to remux the DASH segments. Tidal serves HiRes-tagged
        // tracks as unencrypted DASH FLAC (up to 24-bit); non-HiRes tracks fall back
        // to AAC. If FLAC is disabled or ffmpeg is missing we ask for HIGH, which
        // keeps the original MP4/AAC behaviour with no ffmpeg dependency.
        #[cfg(not(target_os = "windows"))]
        let quality = if crate::config::load().hires_flac && ffmpeg_available() {
            "HI_RES_LOSSLESS"
        } else {
            "HIGH"
        };

        let resp = self.get(
            &format!("tracks/{}/playbackinfopostpaywall", id),
            &[
                ("audioquality", quality),
                ("playbackmode", "STREAM"),
                ("assetpresentation", "FULL"),
                ("prefetchlevel", "NONE"),
                ("countryCode", &self.session.country_code),
            ],
        )?;

        #[derive(Deserialize)]
        struct PlaybackInfo {
            #[serde(rename = "manifestMimeType")]
            manifest_mime_type: String,
            manifest: String,
        }
        #[derive(Deserialize)]
        struct BtsManifest {
            urls: Vec<String>,
            #[serde(rename = "encryptionType", default)]
            encryption_type: String,
            #[serde(rename = "keyId")]
            key_id: Option<String>,
        }

        let info: PlaybackInfo = resp.json()?;
        let decoded = base64::engine::general_purpose::STANDARD.decode(&info.manifest)
            .map_err(|e| anyhow!("base64 decode: {}", e))?;

        if info.manifest_mime_type.contains("bts") {
            let bts: BtsManifest = serde_json::from_slice(&decoded)
                .map_err(|e| anyhow!("BTS manifest parse: {}", e))?;
            let url = bts.urls.into_iter().next()
                .ok_or_else(|| anyhow!("Empty URL list in manifest"))?;
            let encryption = if bts.encryption_type == "OLD_AES" {
                let key_id = bts.key_id.ok_or_else(|| anyhow!("Missing keyId for encrypted track"))?;
                Some(decrypt_security_token(&key_id)?)
            } else {
                None
            };
            Ok(StreamInfo::Single { url, encryption })
        } else if info.manifest_mime_type.contains("dash") {
            let xml = String::from_utf8_lossy(&decoded);
            // HiRes FLAC is delivered as a multi-segment DASH SegmentTemplate.
            if let Some(segments) = parse_dash_segments(&xml) {
                Ok(StreamInfo::Dash { segments })
            } else {
                // Old-style single-segment DASH: a lone <BaseURL>.
                let url = xml.lines()
                    .find(|l| l.contains("<BaseURL>"))
                    .and_then(|l| l.split("<BaseURL>").nth(1))
                    .and_then(|l| l.split("</BaseURL>").next())
                    .map(|s| s.trim().to_string())
                    .ok_or_else(|| anyhow!("Could not extract segments or BaseURL from DASH manifest"))?;
                Ok(StreamInfo::Single { url, encryption: None })
            }
        } else {
            Err(anyhow!("Unknown manifest type: {}", info.manifest_mime_type))
        }
    }

    // ── Search ───────────────────────────────────────────────────────────────

    pub fn search_tracks(&self, query: &str, limit: u32) -> Result<Vec<TrackInfo>> {
        #[derive(Deserialize)]
        struct SearchResp {
            tracks: Option<SearchItems>,
        }
        #[derive(Deserialize)]
        struct SearchItems {
            items: Vec<RawTrack>,
        }

        let limit_s = limit.to_string();
        let resp = self.get("search", &[
            ("query", query),
            ("limit", &limit_s),
            ("types", "TRACKS"),
            ("countryCode", &self.session.country_code),
        ])?;
        let data: SearchResp = resp.json()?;
        Ok(data.tracks.map(|t| t.items.into_iter().map(Into::into).collect())
            .unwrap_or_default())
    }

    pub fn search_artists(&self, query: &str) -> Result<Vec<ArtistInfo>> {
        #[derive(Deserialize)]
        struct SearchResp {
            artists: Option<SearchItems>,
        }
        #[derive(Deserialize)]
        struct SearchItems {
            items: Vec<RawArtist>,
        }

        let resp = self.get("search", &[
            ("query", query),
            ("limit", "5"),
            ("types", "ARTISTS"),
            ("countryCode", &self.session.country_code),
        ])?;
        let data: SearchResp = resp.json()?;
        Ok(data.artists.map(|a| a.items.into_iter()
            .map(|r| ArtistInfo { id: r.id, name: r.name })
            .collect())
            .unwrap_or_default())
    }

    pub fn artist_top_tracks(&self, artist_id: u64, limit: u32) -> Result<Vec<TrackInfo>> {
        #[derive(Deserialize)]
        struct Resp {
            items: Vec<RawTrack>,
        }
        let limit_s = limit.to_string();
        let resp = self.get(
            &format!("artists/{}/toptracks", artist_id),
            &[("limit", &limit_s), ("countryCode", &self.session.country_code)],
        )?;
        let data: Resp = resp.json()?;
        Ok(data.items.into_iter().map(Into::into).collect())
    }

    // ── Mixes ────────────────────────────────────────────────────────────────

    pub fn mixes(&self) -> Result<Vec<MixInfo>> {
        #[derive(Deserialize)]
        struct PageResp {
            rows: Option<Vec<PageRow>>,
        }
        #[derive(Deserialize)]
        struct PageRow {
            modules: Option<Vec<PageModule>>,
        }
        #[derive(Deserialize)]
        struct PageModule {
            #[serde(rename = "pagedList")]
            paged_list: Option<PagedList>,
        }
        #[derive(Deserialize)]
        struct PagedList {
            items: Vec<RawMix>,
        }
        #[derive(Deserialize)]
        struct RawMix {
            id: String,
            title: String,
            #[serde(rename = "mixType", default)]
            mix_type: String,
        }

        let resp = self.get("pages/my_collection_my_mixes", &[
            ("deviceType", "BROWSER"),
            ("locale", "en_US"),
            ("countryCode", &self.session.country_code),
        ])?;
        let data: PageResp = resp.json()?;

        let mut mixes = Vec::new();
        if let Some(rows) = data.rows {
            for row in rows {
                if let Some(modules) = row.modules {
                    for module in modules {
                        if let Some(paged) = module.paged_list {
                            for m in paged.items {
                                if !m.mix_type.contains("VIDEO") {
                                    mixes.push(MixInfo { id: m.id, title: m.title });
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(mixes)
    }

    pub fn mix_tracks(&self, mix_id: &str) -> Result<Vec<TrackInfo>> {
        #[derive(Deserialize)]
        struct Resp {
            items: Vec<MixItem>,
        }
        #[derive(Deserialize)]
        struct MixItem {
            #[serde(rename = "type")]
            item_type: String,
            item: Option<RawTrack>,
        }

        let resp = self.get(
            &format!("mixes/{}/items", mix_id),
            &[
                ("limit", "50"),
                ("deviceType", "BROWSER"),
                ("countryCode", &self.session.country_code),
            ],
        )?;
        let data: Resp = resp.json()?;
        Ok(data.items.into_iter()
            .filter(|i| i.item_type == "track")
            .filter_map(|i| i.item.map(Into::into))
            .collect())
    }

    // ── Playlists ────────────────────────────────────────────────────────────

    pub fn playlists(&self) -> Result<Vec<PlaylistInfo>> {
        #[derive(Deserialize)]
        struct Resp {
            items: Vec<RawPlaylist>,
        }
        #[derive(Deserialize)]
        struct RawPlaylist {
            uuid: String,
            title: String,
        }

        let resp = self.get(
            &format!("users/{}/playlists", self.session.user_id),
            &[("countryCode", &self.session.country_code), ("limit", "50")],
        )?;
        let data: Resp = resp.json()?;
        Ok(data.items.into_iter()
            .map(|p| PlaylistInfo { id: p.uuid, title: p.title })
            .collect())
    }

    pub fn playlist_tracks(&self, playlist_id: &str) -> Result<Vec<TrackInfo>> {
        #[derive(Deserialize)]
        struct Resp {
            items: Vec<PlaylistItem>,
        }
        #[derive(Deserialize)]
        struct PlaylistItem {
            #[serde(rename = "type")]
            item_type: String,
            item: Option<RawTrack>,
        }

        let resp = self.get(
            &format!("playlists/{}/items", playlist_id),
            &[
                ("limit", "50"),
                ("countryCode", &self.session.country_code),
            ],
        )?;
        let data: Resp = resp.json()?;
        Ok(data.items.into_iter()
            .filter(|i| i.item_type == "track")
            .filter_map(|i| i.item.map(Into::into))
            .collect())
    }

    // ── Radio ────────────────────────────────────────────────────────────────

    pub fn track_radio(&self, track_id: u64) -> Result<Vec<TrackInfo>> {
        #[derive(Deserialize)]
        struct Resp {
            items: Vec<RawTrack>,
        }

        let resp = self.get(
            &format!("tracks/{}/radio", track_id),
            &[("limit", "50"), ("countryCode", &self.session.country_code)],
        )?;
        let data: Resp = resp.json()?;
        Ok(data.items.into_iter().map(Into::into).collect())
    }

    // ── Library (favorites) ────────────────────────────────────────────────

    pub fn album_tracks(&self, album_id: u64) -> Result<Vec<TrackInfo>> {
        #[derive(Deserialize)]
        struct Resp {
            items: Vec<RawTrack>,
        }

        let resp = self.get(
            &format!("albums/{}/tracks", album_id),
            &[("countryCode", &self.session.country_code), ("limit", "100")],
        )?;
        let data: Resp = resp.json()?;
        Ok(data.items.into_iter().map(Into::into).collect())
    }

    // ── Library (page-level, for streaming) ────────────────────────────────

    /// Fetch a single page of liked tracks. Returns (tracks, total_count).
    pub fn liked_tracks_page(&self, offset: u64, limit: u64) -> Result<(Vec<TrackInfo>, u64)> {
        #[derive(Deserialize)]
        struct Resp { items: Vec<FavItem>, #[serde(rename = "totalNumberOfItems")] total: Option<u64> }
        #[derive(Deserialize)]
        struct FavItem { item: RawTrack }

        let offset_s = offset.to_string();
        let limit_s = limit.to_string();
        let resp = self.get(
            &format!("users/{}/favorites/tracks", self.session.user_id),
            &[("countryCode", &self.session.country_code), ("limit", &limit_s), ("offset", &offset_s)],
        )?;
        let data: Resp = resp.json()?;
        let total = data.total.unwrap_or(0);
        Ok((data.items.into_iter().map(|f| f.item.into()).collect(), total))
    }

    /// Fetch a single page of favorite albums. Returns (albums, total_count).
    pub fn favorite_albums_page(&self, offset: u64, limit: u64) -> Result<(Vec<AlbumInfo>, u64)> {
        #[derive(Deserialize)]
        struct Resp { items: Vec<FavItem>, #[serde(rename = "totalNumberOfItems")] total: Option<u64> }
        #[derive(Deserialize)]
        struct FavItem { item: RawFavAlbum }
        #[derive(Deserialize)]
        struct RawFavAlbum { id: u64, title: String, artist: Option<RawArtist>, cover: Option<String> }

        let offset_s = offset.to_string();
        let limit_s = limit.to_string();
        let resp = self.get(
            &format!("users/{}/favorites/albums", self.session.user_id),
            &[("countryCode", &self.session.country_code), ("limit", &limit_s), ("offset", &offset_s)],
        )?;
        let data: Resp = resp.json()?;
        let total = data.total.unwrap_or(0);
        let albums = data.items.into_iter().map(|f| AlbumInfo {
            id: f.item.id, title: f.item.title,
            artist_name: f.item.artist.map(|a| a.name).unwrap_or_default(),
            cover: f.item.cover,
        }).collect();
        Ok((albums, total))
    }

    /// Fetch a single page of favorite artists. Returns (artists, total_count).
    pub fn favorite_artists_page(&self, offset: u64, limit: u64) -> Result<(Vec<ArtistInfo>, u64)> {
        #[derive(Deserialize)]
        struct Resp { items: Vec<FavItem>, #[serde(rename = "totalNumberOfItems")] total: Option<u64> }
        #[derive(Deserialize)]
        struct FavItem { item: RawArtist }

        let offset_s = offset.to_string();
        let limit_s = limit.to_string();
        let resp = self.get(
            &format!("users/{}/favorites/artists", self.session.user_id),
            &[("countryCode", &self.session.country_code), ("limit", &limit_s), ("offset", &offset_s)],
        )?;
        let data: Resp = resp.json()?;
        let total = data.total.unwrap_or(0);
        let artists = data.items.into_iter().map(|f| ArtistInfo { id: f.item.id, name: f.item.name }).collect();
        Ok((artists, total))
    }

    // ── Cover image ──────────────────────────────────────────────────────────

    pub fn fetch_cover(&self, cover_id: &str, size: u32) -> Result<Vec<u8>> {
        let path = cover_id.replace('-', "/");
        let url = format!("https://resources.tidal.com/images/{}/{}x{}.jpg", path, size, size);
        let bytes = self.client.get(&url).send()?.bytes()?;
        Ok(bytes.to_vec())
    }
}

// ─── Encryption ──────────────────────────────────────────────────────────────

// Publicly known Tidal master key used to wrap per-track AES keys.
// The same key is used by python-tidal and other open-source clients.
// base64: UIlTTEMmmLfGowo/UC60x2H45W6MdGgTRfo/umg4754=
const TIDAL_MASTER_KEY: &[u8] = &[
    0x50, 0x89, 0x53, 0x4C, 0x43, 0x26, 0x98, 0xB7,
    0xC6, 0xA3, 0x0A, 0x3F, 0x50, 0x2E, 0xB4, 0xC7,
    0x61, 0xF8, 0xE5, 0x6E, 0x8C, 0x74, 0x68, 0x13,
    0x45, 0xFA, 0x3F, 0xBA, 0x68, 0x38, 0xEF, 0x9E,
];

/// Decrypt Tidal's `OLD_AES` security token to recover the per-track AES-128-CTR
/// key (16 bytes) and nonce (8 bytes).
fn decrypt_security_token(key_id: &str) -> Result<EncryptionInfo> {
    use cbc::cipher::{BlockDecryptMut, KeyIvInit, block_padding::NoPadding};
    type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;

    let token = base64::engine::general_purpose::STANDARD.decode(key_id)
        .map_err(|e| anyhow!("keyId base64: {}", e))?;
    if token.len() < 32 {
        return Err(anyhow!("Security token too short ({} bytes)", token.len()));
    }

    let iv = &token[..16];
    let mut buf = token[16..].to_vec();
    // Pad to AES block boundary (should already be aligned, but be safe)
    let rem = buf.len() % 16;
    if rem != 0 { buf.extend(std::iter::repeat(0u8).take(16 - rem)); }

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

// ─── DASH (HiRes FLAC) ───────────────────────────────────────────────────────

/// Parse a Tidal DASH `SegmentTemplate` manifest into an ordered list of segment
/// URLs (the init segment first, then each media segment). Returns `None` if the
/// manifest isn't SegmentTemplate-based, so the caller can fall back to a single
/// `<BaseURL>`.
fn parse_dash_segments(xml: &str) -> Option<Vec<String>> {
    let st = xml.find("<SegmentTemplate")?;
    let tag_end = xml[st..].find('>')? + st;
    let tag = &xml[st..tag_end];

    let init = xml_attr(tag, "initialization")?;
    let media = xml_attr(tag, "media")?;
    let start: u64 = xml_attr(tag, "startNumber").and_then(|v| v.parse().ok()).unwrap_or(1);

    // Count media segments from the SegmentTimeline: each <S> element covers
    // (1 + r) segments, where r ("repeat") defaults to 0.
    let timeline = &xml[xml.find("<SegmentTimeline")?..];
    let mut count: u64 = 0;
    let mut rest = timeline;
    while let Some(p) = rest.find("<S ") {
        let end = rest[p..].find("/>")? + p;
        let s_el = &rest[p..end];
        let r: u64 = xml_attr(s_el, "r").and_then(|v| v.parse().ok()).unwrap_or(0);
        count += 1 + r;
        rest = &rest[end + 2..];
    }
    if count == 0 {
        return None;
    }

    let mut segments = Vec::with_capacity(count as usize + 1);
    segments.push(init.to_string());
    for n in start..start + count {
        segments.push(media.replace("$Number$", &n.to_string()));
    }
    Some(segments)
}

/// Whether `ffmpeg` is on PATH. Probed once and cached — it gates whether we even
/// request the DASH/FLAC tier, so a machine without ffmpeg behaves exactly as before
/// (MP4/AAC) instead of failing on HiRes tracks.
#[cfg(not(target_os = "windows"))]
fn ffmpeg_available() -> bool {
    use std::sync::OnceLock;
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        std::process::Command::new("ffmpeg")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// Read the value of an XML attribute `name="..."` from a single element string.
fn xml_attr<'a>(element: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!("{}=\"", name);
    let start = element.find(&needle)? + needle.len();
    let end = element[start..].find('"')? + start;
    Some(&element[start..end])
}

/// Download an ordered list of DASH segments, concatenate them, and remux to a
/// native FLAC file at `dest` using ffmpeg. This is a lossless stream-copy
/// (`-c:a copy`) — the FLAC audio is never decoded or re-encoded, only rewrapped
/// from the fragmented-MP4 container into a `.flac` container. `dest` should end
/// in `.flac`; `ffmpeg` must be on PATH.
pub fn fetch_dash_flac(segments: &[String], dest: &std::path::Path) -> Result<()> {
    use std::io::Write;
    let http = reqwest::blocking::Client::builder()
        .user_agent(crate::auth::TIDAL_UA)
        .build()?;

    let part = dest.with_extension("mp4.part");
    {
        let mut f = std::fs::File::create(&part)?;
        for seg in segments {
            let bytes = http.get(seg).send()?.error_for_status()?.bytes()?;
            f.write_all(&bytes)?;
        }
    }

    let status = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&part)
        .args(["-c:a", "copy", "-y"])
        .arg(dest)
        .status();
    let _ = std::fs::remove_file(&part);

    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(anyhow!("ffmpeg remux failed (exit {:?})", s.code())),
        Err(e) => Err(anyhow!(
            "ffmpeg not found ({e}). Install ffmpeg to play/download HiRes FLAC on Linux/macOS."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> TrackInfo {
        serde_json::from_str::<RawTrack>(json).unwrap().into()
    }

    #[test]
    fn artist_from_artist_field() {
        let t = parse(r#"{"id":1,"title":"Song","artist":{"id":10,"name":"Main Artist"},"album":{"id":100,"title":"Album"}}"#);
        assert_eq!(t.artist_name, "Main Artist");
    }

    #[test]
    fn artist_falls_back_to_artists_array() {
        let t = parse(r#"{"id":1,"title":"Song","artists":[{"id":10,"name":"First"},{"id":11,"name":"Second"}],"album":{"id":100,"title":"Album"}}"#);
        assert_eq!(t.artist_name, "First");
    }

    #[test]
    fn artist_empty_when_both_absent() {
        let t = parse(r#"{"id":1,"title":"Song","album":{"id":100,"title":"Album"}}"#);
        assert_eq!(t.artist_name, "");
    }

    #[test]
    fn year_parsed_from_release_date() {
        let t = parse(r#"{"id":1,"title":"Song","album":{"id":100,"title":"Album","releaseDate":"2021-05-14"}}"#);
        assert_eq!(t.album_release_year, Some(2021));
    }

    #[test]
    fn year_absent_when_no_release_date() {
        let t = parse(r#"{"id":1,"title":"Song","album":{"id":100,"title":"Album"}}"#);
        assert_eq!(t.album_release_year, None);
    }

    #[test]
    fn track_copyright_takes_precedence_over_album() {
        let t = parse(r#"{"id":1,"title":"Song","copyright":"Track (c)","album":{"id":100,"title":"Album","copyright":"Album (c)"}}"#);
        assert_eq!(t.album_copyright, Some("Track (c)".to_string()));
    }

    #[test]
    fn falls_back_to_album_copyright() {
        let t = parse(r#"{"id":1,"title":"Song","album":{"id":100,"title":"Album","copyright":"Album (c)"}}"#);
        assert_eq!(t.album_copyright, Some("Album (c)".to_string()));
    }

    #[test]
    fn missing_optionals_use_defaults() {
        let t = parse(r#"{"id":42,"title":"Minimal","album":{"id":100,"title":"Album"}}"#);
        assert_eq!(t.id, 42);
        assert_eq!(t.track_num, 1);
        assert_eq!(t.volume_num, 1);
        assert_eq!(t.duration, 0);
        assert_eq!(t.audio_quality, "UNKNOWN");
        assert!(t.isrc.is_none());
    }

    #[test]
    fn album_artist_falls_back_to_primary_artist() {
        let t = parse(r#"{"id":1,"title":"Song","artist":{"id":10,"name":"Solo"},"album":{"id":100,"title":"Album"}}"#);
        assert_eq!(t.album_artist, "Solo");
    }

    /// End-to-end HiRes path: session → stream_url (DASH) → fetch_dash_flac →
    /// validate native FLAC. Run with:
    ///   cargo test e2e_dash_flac -- --ignored --nocapture
    #[test]
    #[ignore = "requires a real Tidal session on disk + ffmpeg"]
    fn e2e_dash_flac_download() {
        let session = crate::auth::get_session().expect("load session");
        let client = TidalClient::new(session);
        let track_id = 19823990u64; // Daft Punk — Get Lucky (HIRES_LOSSLESS tagged)

        let segments = match client.stream_url(track_id).expect("stream_url failed") {
            StreamInfo::Dash { segments } => segments,
            StreamInfo::Single { .. } => panic!("expected DASH HiRes manifest — is this a HiRes account/track?"),
        };
        println!("[1] DASH manifest: {} segments (init + media)", segments.len());
        assert!(segments.len() > 1, "expected init + media segments");

        let tmp = tempfile::Builder::new().suffix(".flac").tempfile().unwrap();
        let dest = tmp.path().to_path_buf();
        fetch_dash_flac(&segments, &dest).expect("fetch_dash_flac failed");

        let mut head = [0u8; 4];
        std::io::Read::read_exact(&mut std::fs::File::open(&dest).unwrap(), &mut head).unwrap();
        let size = std::fs::metadata(&dest).unwrap().len();
        println!("[2] wrote {} bytes, magic={:?}", size, &head);
        assert_eq!(&head, b"fLaC", "output is not a native FLAC file");
        assert!(size > 1_000_000, "FLAC file suspiciously small");
        println!("[3] OK — native FLAC produced");
    }

    #[test]
    fn dash_segments_parsed_from_segment_timeline() {
        // Init segment + two <S> runs: (1 + 61) + (1 + 0) = 63 media segments.
        let xml = r#"<MPD><Period><AdaptationSet><Representation codecs="flac">
            <SegmentTemplate timescale="44100" initialization="https://cdn/init.mp4?t=1"
              media="https://cdn/$Number$.mp4?t=1" startNumber="1">
              <SegmentTimeline><S d="176128" r="61"/><S d="676"/></SegmentTimeline>
            </SegmentTemplate></Representation></AdaptationSet></Period></MPD>"#;
        let segs = parse_dash_segments(xml).expect("should parse");
        assert_eq!(segs.len(), 64); // 1 init + 63 media
        assert_eq!(segs[0], "https://cdn/init.mp4?t=1");
        assert_eq!(segs[1], "https://cdn/1.mp4?t=1");
        assert_eq!(segs[63], "https://cdn/63.mp4?t=1");
    }

    #[test]
    fn dash_segments_none_without_segment_template() {
        let xml = r#"<MPD><Period><BaseURL>https://cdn/single.flac</BaseURL></Period></MPD>"#;
        assert!(parse_dash_segments(xml).is_none());
    }

    #[test]
    fn all_artists_collected() {
        let t = parse(r#"{"id":1,"title":"Song","artists":[{"id":1,"name":"A"},{"id":2,"name":"B"},{"id":3,"name":"C"}],"album":{"id":100,"title":"Album"}}"#);
        assert_eq!(t.artists, vec!["A", "B", "C"]);
    }

    /// End-to-end: session → stream_url → download → decrypt → magic-byte check → symphonia probe.
    /// Run with: cargo test e2e_stream_decrypt -- --ignored --nocapture
    #[test]
    #[ignore = "requires a real Tidal session on disk (~/.config/lumitide/session.json)"]
    fn e2e_stream_decrypt() {
        use std::io::{Read, Write};
        use ctr::cipher::{KeyIvInit, StreamCipher};
        type Aes128Ctr = ctr::Ctr64BE<aes::Aes128>;

        // ── Step 1: session ───────────────────────────────────────────────────
        let session = crate::auth::get_session().expect("load session");
        println!("[1] Session loaded — user_id={} country={}", session.user_id, session.country_code);
        println!("    token type={} expired={}", session.token_type, session.is_expired());

        // ── Step 2: stream_url ────────────────────────────────────────────────
        let track_id = 86430568u64; // Netsky — Escape (known LOSSLESS track)
        let client = TidalClient::new(session.clone());
        let stream_info = client.stream_url(track_id).expect("stream_url failed");
        let (url, encryption) = match stream_info {
            StreamInfo::Single { url, encryption } => (url, encryption),
            StreamInfo::Dash { segments } => {
                println!("[2] DASH HiRes FLAC manifest — {} segments (init + media); \
                          this test only covers the bts/decrypt path", segments.len());
                return;
            }
        };
        println!("[2] stream_url OK");
        println!("    url prefix  : {}", &url[..url.len().min(80)]);
        println!("    encrypted   : {}", encryption.is_some());
        if let Some(ref enc) = encryption {
            println!("    key  (hex)  : {}", enc.key.iter().map(|b| format!("{:02x}", b)).collect::<String>());
            println!("    nonce(hex)  : {}", enc.nonce.iter().map(|b| format!("{:02x}", b)).collect::<String>());
        }

        // ── Step 3: download first 256 KiB ────────────────────────────────────
        let http = reqwest::blocking::Client::builder()
            .user_agent(crate::auth::TIDAL_UA)
            .build().unwrap();
        let mut resp = http.get(&url)
            .header("Range", "bytes=0-262143")
            .send().expect("HTTP GET failed");
        println!("[3] HTTP {} content-length={:?}", resp.status(), resp.content_length());
        assert!(resp.status().is_success(), "CDN returned {}", resp.status());

        let mut raw = Vec::new();
        resp.read_to_end(&mut raw).expect("read body");
        println!("    downloaded {} bytes", raw.len());

        // ── Step 4: decrypt (if needed) ───────────────────────────────────────
        if let Some(enc) = encryption {
            let mut iv = [0u8; 16];
            iv[..8].copy_from_slice(&enc.nonce);
            let mut cipher = Aes128Ctr::new_from_slices(&enc.key, &iv).expect("cipher init");
            cipher.apply_keystream(&mut raw);
            println!("[4] Decrypted {} bytes", raw.len());
        } else {
            println!("[4] No encryption — plaintext");
        }

        // ── Step 5: magic bytes ───────────────────────────────────────────────
        let magic12: &[u8] = &raw[..12.min(raw.len())];
        let ext = crate::utils::audio_extension(magic12);
        println!("[5] Magic bytes : {:02x?}", &raw[..8.min(raw.len())]);
        println!("    Detected ext: {}", ext);
        let is_flac = raw.starts_with(b"fLaC");
        let is_mp4  = raw.len() >= 8 && &raw[4..8] == b"ftyp";
        println!("    is_flac={} is_mp4={}", is_flac, is_mp4);

        // ── Step 6: write to temp file and symphonia-probe it ─────────────────
        let tmp = tempfile::Builder::new().suffix(&format!(".{}", ext)).tempfile().unwrap();
        let tmp_path = tmp.path().to_path_buf();
        {
            let mut f = std::fs::File::create(&tmp_path).unwrap();
            f.write_all(&raw).unwrap();
        }
        println!("[6] Wrote {} bytes to {}", raw.len(), tmp_path.display());

        use symphonia::core::io::MediaSourceStream;
        use symphonia::core::formats::FormatOptions;
        use symphonia::core::meta::MetadataOptions;
        use symphonia::core::probe::Hint;
        let file = std::fs::File::open(&tmp_path).unwrap();
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        hint.with_extension(ext);
        match symphonia::default::get_probe().format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default()) {
            Ok(probed) => {
                let tracks = probed.format.tracks();
                println!("[6] Symphonia probed OK — {} track(s)", tracks.len());
                for t in tracks {
                    println!("    codec={:?} sample_rate={:?} channels={:?}",
                        t.codec_params.codec,
                        t.codec_params.sample_rate,
                        t.codec_params.channels);
                }
            }
            Err(e) => println!("[6] Symphonia probe FAILED: {}", e),
        }

        assert!(is_flac || is_mp4, "Unrecognised audio format — magic bytes: {:02x?}", &raw[..8.min(raw.len())]);
    }

    /// Probe multiple playback endpoint variants to find which works with the current token.
    /// Run with: cargo test probe_playback -- --ignored --nocapture
    #[test]
    #[ignore = "requires a real Tidal session on disk (~/.config/lumitide/session.json)"]
    fn probe_playback_endpoint() {
        let session = crate::auth::get_session().expect("could not load session");
        let track_id = 86430568u64;
        let http = reqwest::blocking::Client::builder()
            .user_agent(crate::auth::TIDAL_UA)
            .build().unwrap();

        // Variant A: desktop.tidal.com with x-tidal-token (current impl)
        {
            let r = http.get(format!("https://desktop.tidal.com/v1/tracks/{}/playbackinfo", track_id))
                .header("x-tidal-token", &session.access_token)
                .header("x-tidal-streamingsessionid", crate::auth::new_uuid())
                .query(&[("audioquality","LOSSLESS"),("playbackmode","STREAM"),("assetpresentation","FULL"),("countryCode",&session.country_code)])
                .send().unwrap();
            println!("A desktop x-tidal-token:  {} — {}", r.status(), r.text().unwrap_or_default().chars().take(300).collect::<String>());
        }

        // Variant B: desktop.tidal.com with x-tidal-token + Origin header
        {
            let r = http.get(format!("https://desktop.tidal.com/v1/tracks/{}/playbackinfo", track_id))
                .header("x-tidal-token", &session.access_token)
                .header("x-tidal-streamingsessionid", crate::auth::new_uuid())
                .header("Origin", "https://desktop.tidal.com")
                .header("Referer", "https://desktop.tidal.com/")
                .query(&[("audioquality","LOSSLESS"),("playbackmode","STREAM"),("assetpresentation","FULL"),("countryCode",&session.country_code)])
                .send().unwrap();
            println!("B desktop x-tidal-token + Origin:  {} — {}", r.status(), r.text().unwrap_or_default().chars().take(300).collect::<String>());
        }

        // Variant C: api.tidal.com postpaywall with Authorization: Bearer — decode manifest
        {
            let r = http.get(format!("https://api.tidal.com/v1/tracks/{}/playbackinfopostpaywall", track_id))
                .header("Authorization", session.auth_header())
                .query(&[("audioquality","LOSSLESS"),("playbackmode","STREAM"),("assetpresentation","FULL"),("prefetchlevel","NONE"),("countryCode",&session.country_code)])
                .send().unwrap();
            let body = r.text().unwrap_or_default();
            println!("C status + raw: {}", &body.chars().take(200).collect::<String>());
            // decode and print the full manifest
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                if let Some(m) = v["manifest"].as_str() {
                    let decoded = base64::engine::general_purpose::STANDARD.decode(m).unwrap_or_default();
                    println!("C manifest decoded: {}", String::from_utf8_lossy(&decoded));
                }
            }
        }
    }
}
