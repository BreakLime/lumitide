use anyhow::Result;
use dialoguer::Select;
use std::sync::{Arc, Mutex};

use crate::api::TidalClient;
use crate::config;
use crate::preview;
use crate::utils::is_saved;

pub fn run(client: &mut TidalClient, debug: bool) -> Result<()> {
    let mixes = client.mixes().map_err(|e| {
        if e.to_string().contains("Forbidden") {
            anyhow::anyhow!("Could not load mixes (access denied). Make sure you have a Tidal HiFi or HiFi Plus subscription and that Mixes are available in your region.")
        } else {
            e
        }
    })?;

    if mixes.is_empty() {
        println!("No mixes found.");
        return Ok(());
    }

    let titles: Vec<&str> = mixes.iter().map(|m| m.title.as_str()).collect();
    let Some(mix_idx) = Select::new()
        .with_prompt("Select a mix")
        .items(&titles)
        .default(0)
        .report(false)
        .interact_opt()?
    else { return Ok(()); };

    let mix = &mixes[mix_idx];
    let tracks = client.mix_tracks(&mix.id)?;

    if tracks.is_empty() {
        println!("No tracks in this mix.");
        return Ok(());
    }

    let cfg = config::load();
    let volume = Arc::new(Mutex::new(cfg.volume));
    let mut idx: usize = 0;
    let mut direction: Option<&str> = None;

    loop {
        let track = &tracks[idx];
        let saved = is_saved(&cfg.output_dir, &track.artist_name, &track.title);
        let label = format!("{} / {}", idx + 1, tracks.len());

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
            _ => {
                idx = (idx + 1) % tracks.len();
                direction = Some("next");
            }
        }
    }
    Ok(())
}
