use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::api::{TidalClient, TrackInfo};
use crate::auth::Session;
use crate::utils::{is_saved, safe_filename};

struct QueueEntry {
    track: TrackInfo,
    session: Session,
    output_dir: String,
}

pub struct DownloadQueue {
    pending: Arc<Mutex<VecDeque<QueueEntry>>>,
    pub done: Arc<AtomicU64>,
    pub total: Arc<AtomicU64>,
    pub current: Arc<Mutex<Option<String>>>,
}

impl DownloadQueue {
    pub fn new() -> Self {
        let pending: Arc<Mutex<VecDeque<QueueEntry>>> = Arc::new(Mutex::new(VecDeque::new()));
        let done = Arc::new(AtomicU64::new(0));
        let total = Arc::new(AtomicU64::new(0));
        let current: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        let pending_w = pending.clone();
        let done_w = done.clone();
        let current_w = current.clone();

        thread::spawn(move || {
            let http = reqwest::blocking::Client::builder()
                .user_agent("TIDAL_ANDROID/1039 okhttp/3.14.9")
                .build()
                .unwrap_or_default();
            loop {
                let entry = {
                    let mut q = pending_w.lock().unwrap_or_else(|e| e.into_inner());
                    q.pop_front()
                };
                match entry {
                    None => thread::sleep(Duration::from_millis(200)),
                    Some(entry) => {
                        let label = format!("{} - {}", entry.track.artist_name, entry.track.title);
                        *current_w.lock().unwrap_or_else(|e| e.into_inner()) = Some(label);
                        download_track(&entry, &http);
                        *current_w.lock().unwrap_or_else(|e| e.into_inner()) = None;
                        done_w.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        });

        DownloadQueue { pending, done, total, current }
    }

    pub fn push_tracks(&self, tracks: Vec<TrackInfo>, session: Session, output_dir: String) {
        let mut q = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let mut added: u64 = 0;
        for track in tracks {
            if !is_saved(&output_dir, &track.artist_name, &track.title) {
                q.push_back(QueueEntry {
                    track,
                    session: session.clone(),
                    output_dir: output_dir.clone(),
                });
                added += 1;
            }
        }
        drop(q);
        if added > 0 {
            self.total.fetch_add(added, Ordering::Relaxed);
        }
    }

    pub fn status(&self) -> Option<String> {
        let current = self.current.lock().unwrap_or_else(|e| e.into_inner()).clone();
        current.map(|name| {
            let done = self.done.load(Ordering::Relaxed);
            let total = self.total.load(Ordering::Relaxed);
            format!("⬇ {}  {} / {}", name, done + 1, total)
        })
    }
}

fn download_track(entry: &QueueEntry, http: &reqwest::blocking::Client) {
    let out_dir = std::path::Path::new(&entry.output_dir);
    if std::fs::create_dir_all(out_dir).is_err() {
        return;
    }
    if is_saved(&entry.output_dir, &entry.track.artist_name, &entry.track.title) {
        return;
    }

    let client = TidalClient::new(entry.session.clone());
    let url = match client.stream_url(entry.track.id) {
        Ok(u) => u,
        Err(_) => return,
    };

    let filename = safe_filename(&format!(
        "{} - {}.flac",
        entry.track.artist_name, entry.track.title
    ));
    let dest = out_dir.join(&filename);
    let tmp_path = dest.with_extension("tmp");

    let resp = match http.get(&url).send() {
        Ok(r) => r,
        Err(_) => return,
    };
    let mut f = match std::fs::File::create(&tmp_path) {
        Ok(f) => f,
        Err(_) => return,
    };
    let mut reader = match resp.error_for_status() {
        Ok(r) => r,
        Err(_) => {
            let _ = std::fs::remove_file(&tmp_path);
            return;
        }
    };

    let mut buf = vec![0u8; 65_536];
    let mut ok = true;
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if f.write_all(&buf[..n]).is_err() {
                    ok = false;
                    break;
                }
            }
            Err(_) => {
                ok = false;
                break;
            }
        }
    }
    drop(f);

    if !ok {
        let _ = std::fs::remove_file(&tmp_path);
        return;
    }
    if std::fs::rename(&tmp_path, &dest).is_err() {
        let _ = std::fs::remove_file(&tmp_path);
        return;
    }

    let cover = entry.track.album_cover.as_deref()
        .and_then(|id| client.fetch_cover(id, 1280).ok());
    let _ = crate::metadata::embed(&dest, &entry.track, cover.as_deref());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_track(id: u64, artist: &str, title: &str) -> TrackInfo {
        TrackInfo {
            id,
            title: title.to_string(),
            artist_name: artist.to_string(),
            album_name: String::new(),
            album_cover: None,
            album_artist: String::new(),
            album_copyright: None,
            album_release_year: None,
            artists: vec![],
            track_num: 1,
            volume_num: 1,
            isrc: None,
            audio_quality: "LOSSLESS".to_string(),
            duration: 240,
        }
    }

    fn make_session() -> Session {
        Session {
            access_token: "test_token".to_string(),
            refresh_token: "test_refresh".to_string(),
            expiry_time: "2099-01-01T00:00:00Z".to_string(),
            user_id: 1,
            country_code: "US".to_string(),
            token_type: "Bearer".to_string(),
        }
    }

    #[test]
    fn new_queue_starts_idle() {
        let q = DownloadQueue::new();
        assert_eq!(q.done.load(Ordering::Relaxed), 0);
        assert_eq!(q.total.load(Ordering::Relaxed), 0);
        assert!(q.status().is_none());
    }

    #[test]
    fn push_tracks_increments_total() {
        let q = DownloadQueue::new();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().to_string_lossy().to_string();
        let tracks = vec![
            make_track(1, "Artist", "Track One"),
            make_track(2, "Artist", "Track Two"),
        ];
        q.push_tracks(tracks, make_session(), out);
        assert_eq!(q.total.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn push_tracks_skips_already_saved() {
        let q = DownloadQueue::new();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().to_string_lossy().to_string();
        let filename = safe_filename("Artist - Saved Track.flac");
        std::fs::File::create(dir.path().join(filename)).unwrap();
        let tracks = vec![
            make_track(1, "Artist", "Saved Track"),
            make_track(2, "Artist", "New Track"),
        ];
        q.push_tracks(tracks, make_session(), out);
        assert_eq!(q.total.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn status_returns_none_when_idle() {
        let q = DownloadQueue::new();
        assert!(q.status().is_none());
    }
}
