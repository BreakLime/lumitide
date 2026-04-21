use anyhow::Result;
use dialoguer::Select;

use crate::api::TidalClient;
use crate::config;
use crate::preview;
use crate::radio;
use crate::utils::is_saved;

pub fn run(client: &mut TidalClient, query: &str, limit: u32, by_artist: bool) -> Result<()> {
    let cfg = config::load();

    let tracks = if by_artist {
        let artists = client.search_artists(query)?;
        if artists.is_empty() {
            println!("No artists found for \"{}\".", query);
            return Ok(());
        }
        let artist = if artists.len() == 1 {
            artists.into_iter().next().unwrap()
        } else {
            let names: Vec<&str> = artists.iter().map(|a| a.name.as_str()).collect();
            let idx = Select::new()
                .with_prompt("Which artist?")
                .items(&names)
                .default(0)
                .report(false)
                .interact()?;
            artists.into_iter().nth(idx).unwrap()
        };
        client.artist_top_tracks(artist.id, limit)?
    } else {
        client.search_tracks(query, limit)?
    };

    if tracks.is_empty() {
        println!("No results found.");
        return Ok(());
    }

    let mut cursor: usize = 0;
    loop {
        let labels: Vec<String> = tracks.iter()
            .map(|t| format!("{} — {}", t.title, t.artist_name))
            .collect();

        let Some(idx) = Select::new()
            .with_prompt("Select a track to preview")
            .items(&labels)
            .default(cursor)
            .report(false)
            .interact_opt()?
        else { break };

        cursor = idx;
        let track = &tracks[idx];
        let saved = is_saved(&cfg.output_path(), &track.artist_name, &track.title);
        let result = preview::run(client, track.id, false, None, None, saved, None)?;
        if result.starts_with("radio:") {
            if let Ok(id) = result["radio:".len()..].parse::<u64>() {
                radio::run(client, id, false)?;
            }
            break;
        }
    }

    Ok(())
}
