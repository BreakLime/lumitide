use anyhow::Result;
use std::sync::{Arc, Mutex};

use crate::api::TidalClient;
use crate::config;
use crate::preview;
use crate::utils::is_saved;

pub fn run(client: &mut TidalClient, seed_track_id: u64, debug: bool) -> Result<()> {
    let tracks = client.track_radio(seed_track_id)?;

    if tracks.is_empty() {
        return Ok(());
    }

    let cfg = config::load();
    let volume = Arc::new(Mutex::new(cfg.volume));
    let mut idx: usize = 0;
    let mut direction: Option<&str> = None;

    loop {
        let track = &tracks[idx];
        let saved = is_saved(&cfg.output_dir, &track.artist_name, &track.title);
        let label = format!("Radio  {} / {}", idx + 1, tracks.len());

        let result = preview::run(
            client,
            track.id,
            debug,
            Some(label),
            Some(volume.clone()),
            saved,
            direction,
        )?;

        match result.as_str() {
            "prev" => {
                idx = (idx + tracks.len() - 1) % tracks.len();
                direction = Some("prev");
            }
            "quit" => break,
            r if r.starts_with("radio:") => {
                // User pressed r again — re-seed from the new track
                if let Ok(new_id) = r["radio:".len()..].parse::<u64>() {
                    run(client, new_id, debug)?;
                }
                break;
            }
            _ => {
                idx = (idx + 1) % tracks.len();
                direction = Some("next");
            }
        }
    }
    Ok(())
}
