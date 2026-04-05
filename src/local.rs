use anyhow::Result;
use rand::seq::SliceRandom;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::api::TrackInfo;
use crate::config;
use crate::preview;

const SUPPORTED: &[&str] = &["flac", "mp3", "m4a"];

/// Returns `Ok("mixes")` or `Ok("search")` to request the caller navigate there,
/// `Ok("")` for normal exit / back to menu.
pub fn run(debug: bool) -> Result<String> {
    loop {
        let cfg = config::load();
        let dir = std::path::Path::new(&cfg.output_dir);

        let (files, _skipped) = match std::fs::read_dir(dir) {
            Ok(entries) => {
                let all: Vec<_> = entries.flatten().collect();
                let total = all.len();
                let supported: Vec<_> = all
                    .into_iter()
                    .filter(|e| {
                        e.path()
                            .extension()
                            .and_then(|ext| ext.to_str())
                            .map_or(false, |ext| {
                                SUPPORTED.iter().any(|s| ext.eq_ignore_ascii_case(s))
                            })
                    })
                    .map(|e| e.path())
                    .collect();
                let skipped = total - supported.len();
                (supported, skipped)
            }
            Err(_) => (Vec::new(), 0),
        };

        if files.is_empty() {
            return no_files_menu(&cfg.output_dir);
        }

        // Files found — play them
        return play_files(files, debug, &cfg);
    }
}

fn no_files_menu(folder: &str) -> Result<String> {
    use dialoguer::Select;
    use std::io::Write;

    print!("\x1B[2J\x1B[H");
    let _ = std::io::stdout().flush();

    println!("No music files (FLAC, MP3, M4A) found in:\n  {}\n", folder);
    println!("Press 'd' on any track to download it to your folder.\n");

    let options = [
        "Change download folder",
        "Browse my mixes  — play & download with 'd'",
        "Search tracks    — play & download with 'd'",
        "Back",
    ];

    let Some(choice) = Select::new()
        .items(&options)
        .default(0)
        .report(false)
        .interact_opt()?
    else {
        return Ok(String::new());
    };

    match choice {
        0 => {
            config::edit_interactive()?;
            // Loop back — re-read dir with new folder
            run(false)
        }
        1 => Ok("mixes".to_string()),
        2 => Ok("search".to_string()),
        _ => Ok(String::new()),
    }
}

fn play_files(files: Vec<std::path::PathBuf>, debug: bool, cfg: &config::Config) -> Result<String> {

    let mut rng = rand::thread_rng();
    let mut shuffled = files.clone();
    shuffled.shuffle(&mut rng);

    let volume = Arc::new(Mutex::new(cfg.volume));
    let total = shuffled.len();
    let mut idx: usize = 0;
    let mut going_prev = false;
    let mut failed: HashSet<PathBuf> = HashSet::new();

    loop {
        // Skip over permanently failed files
        let start_idx = idx;
        loop {
            if !failed.contains(&shuffled[idx % total]) {
                break;
            }
            idx = if going_prev { (idx + total - 1) % total } else { (idx + 1) % total };
            if idx == start_idx {
                // Wrapped all the way around — every file has failed
                return Ok(String::new());
            }
        }

        let path = &shuffled[idx % total];

        // Guard: non-UTF-8 path
        let path_str = match path.to_str() {
            Some(s) => s,
            None => {
                if debug { eprintln!("warning: skipping file with non-UTF-8 path: {}", path.display()); }
                failed.insert(path.clone());
                idx = if going_prev { (idx + total - 1) % total } else { (idx + 1) % total };
                continue;
            }
        };

        // Guard: metadata read
        let (track, cover_bytes) =
            std::panic::catch_unwind(|| read_local_metadata(path)).unwrap_or_else(|_| {
                if debug { eprintln!("warning: could not read tags from {}, using filename", path.display()); }
                (fallback_track_info(path), None)
            });

        let label = format!("{} / {}", idx + 1, total);

        match preview::run_local(
            path_str,
            &track,
            cover_bytes.as_deref(),
            debug,
            Some(label),
            Some(volume.clone()),
        ) {
            Ok(result) => {
                match result.as_str() {
                    "prev" => { going_prev = true;  idx = (idx + total - 1) % total; }
                    "quit" => return Ok(String::new()),
                    "fail" => {
                        if debug { eprintln!("warning: skipping {} — could not decode audio (DRM-protected?)", path.display()); }
                        failed.insert(path.clone());
                        idx = if going_prev { (idx + total - 1) % total } else { (idx + 1) % total };
                    }
                    _ => { going_prev = false; idx = (idx + 1) % total; }
                }
            }
            Err(e) => {
                if debug { eprintln!("warning: skipping {} — {}", path.display(), e); }
                failed.insert(path.clone());
                idx = if going_prev { (idx + total - 1) % total } else { (idx + 1) % total };
            }
        }
    }
}

fn fallback_track_info(path: &std::path::Path) -> TrackInfo {
    let title = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Unknown")
        .to_string();
    TrackInfo {
        id: 0,
        title,
        artist_name: "Unknown".to_string(),
        album_name: String::new(),
        album_cover: None,
        album_artist: String::new(),
        album_copyright: None,
        album_release_year: None,
        artists: Vec::new(),
        track_num: 0,
        volume_num: 0,
        isrc: None,
        audio_quality: "AUDIO".to_string(),
        duration: 0,
    }
}

fn read_local_metadata(path: &std::path::Path) -> (TrackInfo, Option<Vec<u8>>) {
    use lofty::prelude::*;
    use lofty::picture::PictureType;

    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Unknown")
        .to_string();

    let fmt = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_uppercase())
        .unwrap_or_else(|| "AUDIO".to_string());

    let mut title = stem.clone();
    let mut artist_name = "Unknown".to_string();
    let mut album_name = String::new();
    let mut cover_bytes: Option<Vec<u8>> = None;

    if let Ok(tagged) = lofty::read_from_path(path) {
        let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
        if let Some(tag) = tag {
            if let Some(t) = tag.title() { title = t.into_owned(); }
            if let Some(a) = tag.artist() { artist_name = a.into_owned(); }
            if let Some(al) = tag.album() { album_name = al.into_owned(); }

            cover_bytes = tag
                .pictures()
                .iter()
                .find(|p| p.pic_type() == PictureType::CoverFront)
                .or_else(|| tag.pictures().first())
                .map(|p| p.data().to_vec());
        }
    }

    let track = TrackInfo {
        id: 0,
        title,
        artist_name,
        album_name,
        album_cover: None,
        album_artist: String::new(),
        album_copyright: None,
        album_release_year: None,
        artists: Vec::new(),
        track_num: 0,
        volume_num: 0,
        isrc: None,
        audio_quality: fmt,
        duration: 0,
    };
    (track, cover_bytes)
}
