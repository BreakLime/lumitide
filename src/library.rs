use std::io;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::thread;

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use dialoguer::Select;
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, Paragraph},
    Terminal,
};

use crate::api::TidalClient;
use crate::config;
use crate::preview;
use crate::radio;
use crate::utils::is_saved;

type AppTerminal = Terminal<CrosstermBackend<io::Stdout>>;

fn setup_terminal() -> Result<AppTerminal> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, Clear(ClearType::All), EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn teardown_terminal(terminal: &mut AppTerminal) {
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
}

// ── Fuzzy select TUI ─────────────────────────────────────────────────────────

/// Interactive fuzzy-select driven by ratatui.
///
/// `items` / `labels` are pre-populated (possibly empty) and grow as new
/// batches arrive on `rx`.  Returns `Some((index, filtered_snapshot))` on
/// Enter — where `index` is the position in `items` and `filtered_snapshot`
/// is the ordered list of visible indices at selection time — or `None` on Escape.
fn fuzzy_select<T>(
    prompt: &str,
    empty_msg: &str,
    items: &mut Vec<T>,
    labels: &mut Vec<String>,
    rx: &mpsc::Receiver<Result<Vec<T>>>,
    format_item: &dyn Fn(&T) -> String,
) -> Result<Option<(usize, Vec<usize>)>> {
    let mut terminal = setup_terminal()?;
    let result = fuzzy_select_inner(prompt, empty_msg, items, labels, rx, format_item, &mut terminal);
    teardown_terminal(&mut terminal);
    result
}

fn fuzzy_select_inner<T>(
    prompt: &str,
    empty_msg: &str,
    items: &mut Vec<T>,
    labels: &mut Vec<String>,
    rx: &mpsc::Receiver<Result<Vec<T>>>,
    format_item: &dyn Fn(&T) -> String,
    terminal: &mut AppTerminal,
) -> Result<Option<(usize, Vec<usize>)>> {
    let matcher = SkimMatcherV2::default();
    let mut query = String::new();
    let mut cursor: usize = 0;
    let mut scroll: usize = 0;
    // filtered holds indices into `items` that match the current query
    let mut filtered: Vec<usize> = (0..items.len()).collect();
    let mut loading = true;
    let mut fetch_error = false;

    // Drain any stale key events from previous screens
    while event::poll(Duration::ZERO)? {
        let _ = event::read();
    }

    loop {
        // Pull in any newly-arrived items from the background thread
        loop {
            match rx.try_recv() {
                Ok(Ok(batch)) => {
                    for item in &batch {
                        labels.push(format_item(item));
                    }
                    let base = items.len();
                    items.extend(batch);
                    for i in base..items.len() {
                        if query.is_empty() || matcher.fuzzy_match(&labels[i], &query).is_some() {
                            filtered.push(i);
                        }
                    }
                }
                Ok(Err(_)) => {
                    fetch_error = true;
                    loading = false;
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    loading = false;
                    break;
                }
            }
        }

        // Empty state: loading finished with no items
        if !loading && items.is_empty() {
            let msg = if fetch_error {
                "Failed to load. Check your connection.\n\nPress any key to go back."
            } else {
                empty_msg
            };
            terminal.draw(|f| {
                let area = f.area();
                f.render_widget(Paragraph::new(msg), area);
            })?;
            // Wait for any key to dismiss
            loop {
                if event::poll(Duration::from_millis(100))? {
                    if let Event::Key(key) = event::read()? {
                        if key.kind == KeyEventKind::Press {
                            return Ok(None);
                        }
                    }
                }
            }
        }

        // Clamp cursor
        let flen = filtered.len();
        if flen == 0 {
            cursor = 0;
            scroll = 0;
        } else if cursor >= flen {
            cursor = flen - 1;
        }

        // Render
        terminal.draw(|f| {
            let area = f.area();
            let max_label_w = area.width.saturating_sub(4) as usize; // room for "> "

            let chunks = Layout::vertical([
                Constraint::Length(1), // prompt
                Constraint::Min(1),   // list
            ])
            .split(area);

            // ── Prompt line ──────────────────────────────────────────────
            let prompt_line = Line::from(vec![
                Span::raw(format!("{}: ", prompt)),
                Span::styled(&query, Style::default().add_modifier(Modifier::BOLD)),
                Span::styled("_", Style::default().add_modifier(Modifier::DIM)),
            ]);
            f.render_widget(Paragraph::new(prompt_line), chunks[0]);

            // ── Item list ────────────────────────────────────────────────
            let visible = chunks[1].height as usize;
            let list_items: Vec<ListItem> = filtered
                .iter()
                .enumerate()
                .skip(scroll)
                .take(visible)
                .map(|(i, &orig_idx)| {
                    let label = &labels[orig_idx];
                    let selected = i == cursor;
                    let prefix = if selected { "> " } else { "  " };

                    // Truncate label to terminal width
                    let truncated: String = if label.chars().count() > max_label_w {
                        let t: String = label.chars().take(max_label_w.saturating_sub(3)).collect();
                        format!("{t}...")
                    } else {
                        label.clone()
                    };

                    // Build spans with fuzzy-match highlighting
                    let spans = if !query.is_empty() {
                        if let Some((_score, indices)) =
                            matcher.fuzzy_indices(&truncated, &query)
                        {
                            let bold = Style::default().add_modifier(Modifier::BOLD);
                            let normal = Style::default();
                            let mut spans = vec![Span::raw(prefix.to_string())];
                            for (ci, ch) in truncated.chars().enumerate() {
                                let style = if indices.contains(&ci) { bold } else { normal };
                                spans.push(Span::styled(String::from(ch), style));
                            }
                            spans
                        } else {
                            vec![Span::raw(format!("{prefix}{truncated}"))]
                        }
                    } else {
                        vec![Span::raw(format!("{prefix}{truncated}"))]
                    };

                    let style = if selected {
                        Style::default().add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    };
                    ListItem::new(Line::from(spans)).style(style)
                })
                .collect();

            f.render_widget(List::new(list_items), chunks[1]);

            // ── Loading indicator ────────────────────────────────────────
            if loading || fetch_error {
                let txt = if fetch_error {
                    format!("⚠ Partial results ({} loaded)", items.len())
                } else {
                    format!("Loading... ({})", items.len())
                };
                let w = txt.chars().count() as u16;
                let loading_area = ratatui::layout::Rect::new(
                    area.width.saturating_sub(w),
                    area.height.saturating_sub(1),
                    w,
                    1,
                );
                f.render_widget(
                    Paragraph::new(txt)
                        .style(Style::default().add_modifier(Modifier::DIM))
                        .alignment(Alignment::Right),
                    loading_area,
                );
            }
        })?;

        // Handle input (50ms poll keeps UI responsive to incoming items)
        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Esc => return Ok(None),
                    KeyCode::Enter => {
                        if !filtered.is_empty() {
                            return Ok(Some((filtered[cursor], filtered.clone())));
                        }
                    }
                    KeyCode::Up => {
                        if cursor > 0 {
                            cursor -= 1;
                            if cursor < scroll {
                                scroll = cursor;
                            }
                        }
                    }
                    KeyCode::Down => {
                        if !filtered.is_empty() && cursor + 1 < filtered.len() {
                            cursor += 1;
                            let visible = terminal.size()?.height.saturating_sub(1) as usize;
                            if cursor >= scroll + visible {
                                scroll = cursor - visible + 1;
                            }
                        }
                    }
                    KeyCode::Backspace => {
                        if query.pop().is_some() {
                            refilter(&matcher, labels, &query, &mut filtered);
                            cursor = 0;
                            scroll = 0;
                        }
                    }
                    KeyCode::Char(c) => {
                        query.push(c);
                        refilter(&matcher, labels, &query, &mut filtered);
                        cursor = 0;
                        scroll = 0;
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Rebuild the filtered-indices list from scratch.
fn refilter(
    matcher: &SkimMatcherV2,
    labels: &[String],
    query: &str,
    filtered: &mut Vec<usize>,
) {
    if query.is_empty() {
        *filtered = (0..labels.len()).collect();
    } else {
        let mut scored: Vec<(usize, i64)> = labels
            .iter()
            .enumerate()
            .filter_map(|(i, label)| {
                matcher.fuzzy_match(label, query).map(|score| (i, score))
            })
            .collect();
        scored.sort_by(|a, b| b.1.cmp(&a.1));
        *filtered = scored.into_iter().map(|(i, _)| i).collect();
    }
}

// ── Background fetch helpers ────────────────────────────────────────────────

fn spawn_paginated<T, F>(session: crate::auth::Session, fetch: F) -> mpsc::Receiver<Result<Vec<T>>>
where
    T: Send + 'static,
    F: Fn(&TidalClient, u64, u64) -> Result<(Vec<T>, u64)> + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let client = TidalClient::new(session);
        let mut offset = 0u64;
        let limit = 100u64;
        loop {
            match fetch(&client, offset, limit) {
                Ok((items, total)) => {
                    let len = items.len() as u64;
                    if tx.send(Ok(items)).is_err() { break; }
                    offset += len;
                    if len < limit || offset >= total { break; }
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                    break;
                }
            }
        }
    });
    rx
}

fn static_receiver<T>() -> mpsc::Receiver<Result<Vec<T>>> {
    let (tx, rx) = mpsc::channel();
    drop(tx); // immediately disconnected — fuzzy_select sees loading = false at once
    rx
}

// ── Public entry point ──────────────────────────────────────────────────────

pub fn run(client: &mut TidalClient, debug: bool) -> Result<()> {
    let options = [
        "Liked tracks",
        "Saved albums",
        "Followed artists",
        "Back",
    ];

    loop {
        use std::io::Write;
        print!("\x1B[2J\x1B[H");
        let _ = std::io::stdout().flush();

        let Some(choice) = Select::new()
            .items(&options)
            .default(0)
            .report(false)
            .interact_opt()?
        else {
            return Ok(());
        };

        match choice {
            0 => liked_tracks(client, debug)?,
            1 => saved_albums(client, debug)?,
            2 => followed_artists(client, debug)?,
            _ => return Ok(()),
        }
    }
}

// ── Library views ───────────────────────────────────────────────────────────

fn liked_tracks(client: &mut TidalClient, debug: bool) -> Result<()> {
    let rx = spawn_paginated(client.session.clone(), |c, o, l| c.liked_tracks_page(o, l));
    let mut items: Vec<crate::api::TrackInfo> = Vec::new();
    let mut labels: Vec<String> = Vec::new();
    let cfg = config::load();
    let volume = Arc::new(Mutex::new(cfg.volume));

    let format_track = |t: &crate::api::TrackInfo| format!("{} — {}", t.title, t.artist_name);

    'select: loop {
        let Some((item_idx, filtered)) = fuzzy_select(
            "Search tracks",
            "You don't have any liked tracks yet.\n\nLike some tracks in the Tidal app and they will appear here.\n\nPress any key to go back.",
            &mut items, &mut labels, &rx, &format_track,
        )? else {
            break;
        };

        let mut pos = filtered.iter().position(|&i| i == item_idx).unwrap_or(0);
        let mut direction: Option<&str> = None;

        loop {
            if pos >= filtered.len() {
                break; // end of filtered results → back to fuzzy select
            }
            let track = &items[filtered[pos]];
            let saved = is_saved(&cfg.output_path(), &track.artist_name, &track.title);
            let label = format!("{} / {}", pos + 1, filtered.len());
            let result = preview::run(client, track.id, debug, Some(label), Some(volume.clone()), saved, direction)?;
            match result.as_str() {
                "quit" => break, // back to fuzzy select
                "prev" => {
                    pos = pos.saturating_sub(1);
                    direction = Some("prev");
                }
                r if r.starts_with("radio:") => {
                    if let Ok(id) = r["radio:".len()..].parse::<u64>() {
                        radio::run(client, id, debug)?;
                    }
                    break 'select;
                }
                _ => {
                    pos += 1; // natural end or "next" → advance in filtered list
                    direction = Some("next");
                }
            }
        }
    }

    Ok(())
}

fn saved_albums(client: &mut TidalClient, debug: bool) -> Result<()> {
    let rx = spawn_paginated(client.session.clone(), |c, o, l| c.favorite_albums_page(o, l));
    let mut items: Vec<crate::api::AlbumInfo> = Vec::new();
    let mut labels: Vec<String> = Vec::new();

    let format_album = |a: &crate::api::AlbumInfo| format!("{} — {}", a.title, a.artist_name);

    loop {
        let Some((album_idx, _)) = fuzzy_select(
            "Search albums",
            "You don't have any saved albums yet.\n\nSave some albums in the Tidal app and they will appear here.\n\nPress any key to go back.",
            &mut items, &mut labels, &rx, &format_album,
        )? else {
            return Ok(()); // Esc → back to library menu
        };

        let album = &items[album_idx];
        let tracks = client.album_tracks(album.id)?;

        if tracks.is_empty() {
            println!("No tracks in this album.");
            continue; // back to album list
        }

        use std::io::Write;
        print!("\x1B[2J\x1B[H");
        let _ = std::io::stdout().flush();

        let album_options = ["Play album", "Download entire album", "Back"];
        let Some(album_choice) = dialoguer::Select::new()
            .with_prompt(&format!("{} — {}", album.title, album.artist_name))
            .items(&album_options)
            .default(0)
            .report(false)
            .interact_opt()?
        else {
            continue;
        };

        if album_choice == 1 {
            download_album(client, album, &tracks)?;
            continue;
        }

        if album_choice == 2 {
            continue;
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
                "quit" => break, // back to album list
                r if r.starts_with("radio:") => {
                    if let Ok(id) = r["radio:".len()..].parse::<u64>() {
                        radio::run(client, id, debug)?;
                    }
                    return Ok(());
                }
                _ => {
                    idx = (idx + 1) % tracks.len();
                    direction = Some("next");
                }
            }
        }
        // outer loop → back to album list
    }
}

fn followed_artists(client: &mut TidalClient, debug: bool) -> Result<()> {
    let rx = spawn_paginated(client.session.clone(), |c, o, l| c.favorite_artists_page(o, l));
    let mut items: Vec<crate::api::ArtistInfo> = Vec::new();
    let mut labels: Vec<String> = Vec::new();

    let format_artist = |a: &crate::api::ArtistInfo| a.name.clone();

    let Some((artist_idx, _)) = fuzzy_select(
        "Search artists",
        "You don't follow any artists yet.\n\nFollow some artists in the Tidal app and they will appear here.\n\nPress any key to go back.",
        &mut items, &mut labels, &rx, &format_artist,
    )? else {
        return Ok(());
    };

    let artist = &items[artist_idx];
    let tracks = client.artist_top_tracks(artist.id, 20)?;

    if tracks.is_empty() {
        println!("No top tracks found for this artist.");
        return Ok(());
    }

    let cfg = config::load();
    let volume = Arc::new(Mutex::new(cfg.volume));
    let track_rx = static_receiver::<crate::api::TrackInfo>();
    let mut track_items = tracks;
    let mut track_labels: Vec<String> = track_items
        .iter()
        .map(|t| format!("{} — {}", t.title, t.artist_name))
        .collect();

    let format_track = |t: &crate::api::TrackInfo| format!("{} — {}", t.title, t.artist_name);

    'select: loop {
        let Some((item_idx, filtered)) = fuzzy_select(
            "Search tracks", "",
            &mut track_items, &mut track_labels, &track_rx, &format_track,
        )? else {
            break;
        };

        let mut pos = filtered.iter().position(|&i| i == item_idx).unwrap_or(0);
        let mut direction: Option<&str> = None;

        loop {
            if pos >= filtered.len() {
                break; // end of filtered results → back to fuzzy select
            }
            let track = &track_items[filtered[pos]];
            let saved = is_saved(&cfg.output_path(), &track.artist_name, &track.title);
            let label = format!("{} / {}", pos + 1, filtered.len());
            let result = preview::run(client, track.id, debug, Some(label), Some(volume.clone()), saved, direction)?;
            match result.as_str() {
                "quit" => break, // back to fuzzy select
                "prev" => {
                    pos = pos.saturating_sub(1);
                    direction = Some("prev");
                }
                r if r.starts_with("radio:") => {
                    if let Ok(id) = r["radio:".len()..].parse::<u64>() {
                        radio::run(client, id, debug)?;
                    }
                    break 'select;
                }
                _ => {
                    pos += 1; // natural end or "next" → advance
                    direction = Some("next");
                }
            }
        }
    }

    Ok(())
}

fn download_album(client: &mut TidalClient, album: &crate::api::AlbumInfo, tracks: &[crate::api::TrackInfo]) -> Result<()> {
    let cfg = crate::config::load();
    let out_dir = std::path::Path::new(&cfg.output_dir);
    std::fs::create_dir_all(out_dir)?;

    let cover_bytes = album.cover.as_ref().and_then(|id| {
        client.fetch_cover(id, 1280).ok()
    });

    println!("\nDownloading album: {} by {}", album.title, album.artist_name);

    for (i, track) in tracks.iter().enumerate() {
        let filename = crate::utils::safe_filename(&format!("{:02}. {} - {}.flac", track.track_num, track.artist_name, track.title));
        let dest = out_dir.join(&filename);

        if crate::utils::is_saved(&cfg.output_path(), &track.artist_name, &track.title) {
            println!("[{}/{}] Skipping (already exists): {}", i + 1, tracks.len(), filename);
            continue;
        }

        print!("[{}/{}] Downloading: {} ... ", i + 1, tracks.len(), filename);
        use std::io::Write;
        let _ = std::io::stdout().flush();

        if client.session.is_expired() {
            if let Ok(new_session) = crate::auth::refresh_token(&client.session.refresh_token) {
                client.session = new_session;
                let _ = crate::auth::save_session(&client.session);
            }
        }

        let mut stream_url_res = client.stream_url(track.id);
        if let Err(e) = &stream_url_res {
            if e.to_string().contains("401") || e.to_string().contains("403") {
                if let Ok(new_session) = crate::auth::refresh_token(&client.session.refresh_token) {
                    client.session = new_session;
                    let _ = crate::auth::save_session(&client.session);
                    stream_url_res = client.stream_url(track.id);
                }
            }
        }

        match stream_url_res {
            Ok(url) => {
                let mut resp = reqwest::blocking::Client::new().get(&url).send();
                if let Ok(ref r) = resp {
                    if r.status().as_u16() == 401 || r.status().as_u16() == 403 {
                        if let Ok(new_session) = crate::auth::refresh_token(&client.session.refresh_token) {
                            client.session = new_session;
                            let _ = crate::auth::save_session(&client.session);
                            if let Ok(new_url) = client.stream_url(track.id) {
                                resp = reqwest::blocking::Client::new().get(&new_url).send();
                            }
                        }
                    }
                }

                match resp {
                    Ok(mut r) => {
                        if r.status().is_success() {
                            if let Ok(mut f) = std::fs::File::create(&dest) {
                                let total_size = r.content_length().unwrap_or(0);
                                let mut downloaded = 0;
                                let mut buf = [0; 8192];
                                use std::io::Read;
                                loop {
                                    match r.read(&mut buf) {
                                        Ok(0) => break,
                                        Ok(n) => {
                                            if f.write_all(&buf[..n]).is_err() {
                                                break;
                                            }
                                            downloaded += n as u64;
                                            let pct = (downloaded as f64 / total_size as f64) * 100.0;
                                            print!("\r[{}/{}] Downloading: {} ... {:.1}%", i + 1, tracks.len(), filename, pct);
                                            let _ = std::io::stdout().flush();
                                        }
                                        Err(_) => break,
                                    }
                                }
                                let _ = crate::metadata::embed(&dest, track, cover_bytes.as_deref());
                                println!("\r[{}/{}] Downloading: {} ... Done!        ", i + 1, tracks.len(), filename);
                            } else {
                                println!("\nFailed to create file");
                            }
                        } else {
                            println!("\nHTTP error {}", r.status());
                        }
                    }
                    Err(e) => println!("\nFailed to download: {}", e),
                }
            }
            Err(e) => println!("\nAPI streaming error: {}", e),
        }
    }
    
    println!("\nAlbum download complete. Press Enter to continue.");
    let mut s = String::new();
    let _ = std::io::stdin().read_line(&mut s);

    Ok(())
}
