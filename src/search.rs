use anyhow::Result;
use dialoguer::Select;
use std::io;
use std::time::Duration;

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::Rect,
    style::{Modifier, Style},
    text::Line,
    widgets::{List, ListItem, Paragraph},
    Terminal,
};

use crate::api::TidalClient;
use crate::config;
use crate::preview;
use crate::radio;
use crate::utils::is_saved;

enum Action { Play(usize), Queue(usize), Back }

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

    let labels: Vec<String> = tracks
        .iter()
        .map(|t| format!("{} — {}", t.title, t.artist_name))
        .collect();

    let show_hint = cfg.show_controls_hint;
    let mut cursor: usize = 0;
    let mut scroll: usize = 0;

    loop {
        enable_raw_mode()?;
        let mut terminal = {
            let mut stdout = io::stdout();
            execute!(stdout, Clear(ClearType::All), EnterAlternateScreen)?;
            Terminal::new(CrosstermBackend::new(stdout))?
        };
        while event::poll(Duration::ZERO)? {
            let _ = event::read();
        }

        let action = loop {
            terminal.draw(|f| {
                let area = f.area();
                let visible = area.height as usize;

                if cursor < scroll { scroll = cursor; }
                if visible > 0 && cursor >= scroll + visible { scroll = cursor + 1 - visible; }

                let items: Vec<ListItem> = labels
                    .iter()
                    .enumerate()
                    .skip(scroll)
                    .take(visible)
                    .map(|(i, label)| {
                        if i == cursor {
                            ListItem::new(Line::from(format!("> {}", label)))
                                .style(Style::default().add_modifier(Modifier::BOLD))
                        } else {
                            ListItem::new(format!("  {}", label))
                        }
                    })
                    .collect();
                f.render_widget(List::new(items), area);

                if let Some(txt) = crate::DOWNLOAD_QUEUE
                    .get()
                    .and_then(|q| q.status())
                    .or_else(|| show_hint.then(|| " ↵ play  d · download ".to_string()))
                {
                    let w = txt.chars().count() as u16;
                    let qs_area = Rect::new(
                        area.width.saturating_sub(w),
                        area.height.saturating_sub(1),
                        w.min(area.width),
                        1,
                    );
                    f.render_widget(
                        Paragraph::new(txt).style(Style::default().add_modifier(Modifier::DIM)),
                        qs_area,
                    );
                }
            })?;

            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind != KeyEventKind::Press { continue; }
                    match key.code {
                        KeyCode::Up | KeyCode::Char('k') => { if cursor > 0 { cursor -= 1; } }
                        KeyCode::Down | KeyCode::Char('j') => { if cursor + 1 < tracks.len() { cursor += 1; } }
                        KeyCode::Enter => break Action::Play(cursor),
                        KeyCode::Char('d') | KeyCode::Char('D') => break Action::Queue(cursor),
                        KeyCode::Esc | KeyCode::Char('q') => break Action::Back,
                        _ => {}
                    }
                }
            }
        };

        let _ = disable_raw_mode();
        let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
        drop(terminal);

        match action {
            Action::Back => return Ok(()),
            Action::Queue(idx) => {
                if let Some(queue) = crate::DOWNLOAD_QUEUE.get() {
                    queue.push_tracks(vec![tracks[idx].clone()], client.session.clone(), cfg.output_path());
                }
            }
            Action::Play(idx) => {
                let track = &tracks[idx];
                let saved = is_saved(&cfg.output_path(), &track.artist_name, &track.title);
                let result = preview::run(client, track.id, false, None, None, saved, None)?;
                if result.starts_with("radio:") {
                    if let Ok(id) = result["radio:".len()..].parse::<u64>() {
                        radio::run(client, id, false)?;
                    }
                    return Ok(());
                }
            }
        }
    }
}
