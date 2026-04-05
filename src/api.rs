use anyhow::{anyhow, Result};
use base64::Engine;
use serde::Deserialize;

use crate::auth::Session;

const API_BASE: &str = "https://api.tidal.com/v1";

// ─── Public data types ────────────────────────────────────────────────────────

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
            .user_agent("TIDAL_ANDROID/1039 okhttp/3.14.9")
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
            return Err(anyhow!("API error {}: {}", resp.status(), path));
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

    pub fn stream_url(&self, id: u64) -> Result<String> {
        let resp = self.get(
            &format!("tracks/{}/playbackinfopostpaywall", id),
            &[
                ("audioquality", "LOSSLESS"),
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
        }
        let info: PlaybackInfo = resp.json()?;
        let decoded = base64::engine::general_purpose::STANDARD.decode(&info.manifest)
            .map_err(|e| anyhow!("base64 decode: {}", e))?;

        if info.manifest_mime_type.contains("bts") {
            let bts: BtsManifest = serde_json::from_slice(&decoded)
                .map_err(|e| anyhow!("BTS manifest parse: {}", e))?;
            bts.urls.into_iter().next().ok_or_else(|| anyhow!("Empty URL list in manifest"))
        } else if info.manifest_mime_type.contains("dash") {
            // Basic DASH: extract first BaseURL from XML
            let xml = String::from_utf8_lossy(&decoded);
            let url = xml.lines()
                .find(|l| l.contains("<BaseURL>"))
                .and_then(|l| l.split("<BaseURL>").nth(1))
                .and_then(|l| l.split("</BaseURL>").next())
                .map(|s| s.trim().to_string())
                .ok_or_else(|| anyhow!("Could not extract BaseURL from DASH manifest"))?;
            Ok(url)
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
                                mixes.push(MixInfo { id: m.id, title: m.title });
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

    // ── Cover image ──────────────────────────────────────────────────────────

    pub fn fetch_cover(&self, cover_id: &str, size: u32) -> Result<Vec<u8>> {
        let path = cover_id.replace('-', "/");
        let url = format!("https://resources.tidal.com/images/{}/{}x{}.jpg", path, size, size);
        let bytes = self.client.get(&url).send()?.bytes()?;
        Ok(bytes.to_vec())
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

    #[test]
    fn all_artists_collected() {
        let t = parse(r#"{"id":1,"title":"Song","artists":[{"id":1,"name":"A"},{"id":2,"name":"B"},{"id":3,"name":"C"}],"album":{"id":100,"title":"Album"}}"#);
        assert_eq!(t.artists, vec!["A", "B", "C"]);
    }
}
