use anyhow::Result;
use dialoguer::Select;
use std::sync::{Arc, Mutex};

use crate::api::TidalClient;
use crate::config;
use crate::preview;
use crate::radio;
use crate::utils::is_saved;

pub fn run(client: &mut TidalClient, debug: bool) -> Result<()> {
    let playlists = client.playlists()?;

    if playlists.is_empty() {
        use std::io::Write;
        print!("\x1B[2J\x1B[H");
        let _ = std::io::stdout().flush();
        println!("You don't have any playlists yet.\n");
        println!("Create a playlist in the Tidal app and it will appear here.\n");
        dialoguer::Select::new()
            .items(&["Back"])
            .default(0)
            .report(false)
            .interact_opt()?;
        return Ok(());
    }

    let titles: Vec<&str> = playlists.iter().map(|p| p.title.as_str()).collect();
    let Some(idx) = Select::new()
        .with_prompt("Select a playlist")
        .items(&titles)
        .default(0)
        .report(false)
        .interact_opt()?
    else { return Ok(()); };

    let playlist = &playlists[idx];
    let tracks = client.playlist_tracks(&playlist.id)?;

    if tracks.is_empty() {
        println!("No tracks in this playlist.");
        return Ok(());
    }

    let pl_action = {
        use std::io::Write;
        print!("\x1B[2J\x1B[H");
        let _ = std::io::stdout().flush();
        let actions = ["Play", "Queue for download"];
        dialoguer::Select::new()
            .with_prompt(&playlist.title)
            .items(&actions)
            .default(0)
            .report(false)
            .interact_opt()?
    };

    match pl_action {
        None => return Ok(()),
        Some(1) => {
            if let Some(queue) = crate::DOWNLOAD_QUEUE.get() {
                let cfg = config::load();
                queue.push_tracks(tracks, client.session.clone(), cfg.output_path());
            }
            return Ok(());
        }
        Some(_) => {}
    }

    let cfg = config::load();
    let volume = Arc::new(Mutex::new(cfg.volume));
    let mut idx: usize = 0;
    let mut direction: Option<&str> = None;

    loop {
        let track = &tracks[idx];
        let saved = is_saved(&cfg.output_path(), &track.artist_name, &track.title);
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
            r if r.starts_with("radio:") => {
                if let Ok(id) = r["radio:".len()..].parse::<u64>() {
                    radio::run(client, id, debug)?;
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
