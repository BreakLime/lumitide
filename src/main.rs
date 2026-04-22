mod api;
mod auth;
mod color_state;
mod config;
mod cover;
mod download_queue;
mod library;
mod local;
mod metadata;
mod mix;
mod panel;
mod playlist;
mod radio;
mod preview;
mod search;
mod spectrum;
mod utils;

use std::sync::OnceLock;

pub static DOWNLOAD_QUEUE: OnceLock<crate::download_queue::DownloadQueue> = OnceLock::new();

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "lumitide", about = "Lumitide — Tidal CLI music player")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Search for tracks (or artist top-tracks with -a)
    Search {
        #[arg(help = "Search query, e.g. \"Netsky\" or \"Chase The Sun\"")]
        query: Option<String>,
        #[arg(short = 'n', long, default_value_t = 10, help = "Number of results")]
        limit: u32,
        #[arg(short, long, help = "Search by artist and show their top tracks")]
        artist: bool,
    },
    /// Browse and play curated Tidal mixes
    Mix {
        #[arg(long, hide = true)]
        debug: bool,
    },
    /// Browse and play your Tidal playlists
    Playlist {
        #[arg(long, hide = true)]
        debug: bool,
    },
    /// Browse your library (liked tracks, saved albums, followed artists)
    Library {
        #[arg(long, hide = true)]
        debug: bool,
    },
    /// Shuffle and play local audio files (FLAC, MP3, M4A)
    Local {
        #[arg(long, hide = true)]
        debug: bool,
    },
    /// Open the config file in your default editor
    Config,
}

fn main() -> Result<()> {
    let _ = DOWNLOAD_QUEUE.set(crate::download_queue::DownloadQueue::new());
    let cli = Cli::parse();

    match cli.command {
        None => interactive_menu(),

        Some(Commands::Search { query, limit, artist }) => {
            let query = query.unwrap_or_else(|| {
                Cli::command()
                    .find_subcommand_mut("search")
                    .expect("search subcommand must exist")
                    .error(
                        clap::error::ErrorKind::MissingRequiredArgument,
                        "Please provide a search query.\n\nExamples:\n  lumitide search <query>\n  lumitide search <artist> -a",
                    )
                    .exit();
            });
            let session = auth::get_session()?;
            let mut client = api::TidalClient::new(session);
            search::run(&mut client, &query, limit, artist)
        }

        Some(Commands::Mix { debug }) => {
            let session = auth::get_session()?;
            let mut client = api::TidalClient::new(session);
            mix::run(&mut client, debug)
        }

        Some(Commands::Playlist { debug }) => {
            let session = auth::get_session()?;
            let mut client = api::TidalClient::new(session);
            playlist::run(&mut client, debug)
        }

        Some(Commands::Library { debug }) => {
            let session = auth::get_session()?;
            let mut client = api::TidalClient::new(session);
            library::run(&mut client, debug)
        }

        Some(Commands::Local { debug }) => local::run(debug).map(|_| ()),

        Some(Commands::Config) => config::open_editor(),
    }
}

fn interactive_menu() -> Result<()> {
    use crossterm::{
        event::{self, Event, KeyCode, KeyEventKind},
        execute,
        terminal::{
            disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen,
            LeaveAlternateScreen,
        },
    };
    use ratatui::{
        backend::CrosstermBackend,
        layout::Rect,
        style::{Color, Modifier, Style},
        text::{Line, Span},
        widgets::{List, ListItem, Paragraph},
        Terminal,
    };
    use std::io;
    use std::time::Duration;

    // First-run: prompt for download folder if still at the default
    {
        let cfg = config::load();
        if cfg.output_dir == "." {
            print!("\x1B[2J\x1B[H");
            use std::io::Write;
            let _ = std::io::stdout().flush();
            println!("Welcome to lumitide!\n");
            println!("Select a folder where your downloaded music will be saved.\n");
            let options = ["Select folder", "Do it later via Config"];
            if let Some(0) = dialoguer::Select::new()
                .items(&options)
                .default(0)
                .report(false)
                .interact_opt()?
            {
                #[cfg(windows)]
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    let mut cfg = cfg;
                    cfg.output_dir = path.to_string_lossy().into_owned();
                    config::save(&cfg)?;
                }
                #[cfg(not(windows))]
                {
                    let input: String = dialoguer::Input::new()
                        .with_prompt("Download folder")
                        .allow_empty(true)
                        .interact_text()?;
                    if !input.trim().is_empty() {
                        let mut cfg = cfg;
                        cfg.output_dir = input.trim().to_owned();
                        config::save(&cfg)?;
                    }
                }
            }
        }
    }

    const BANNER_LINES: &[&str] = &["   <> <> <>", "      <>", "", "L U M I T I D E", ""];
    let accent = {
        let cfg = config::load();
        if cfg.pywal {
            color_state::load_pywal_palette()
                .and_then(|p| p.get(1).copied())
                .unwrap_or((0, 200, 200))
        } else {
            (0, 200, 200)
        }
    };
    let accent_color = Color::Rgb(accent.0, accent.1, accent.2);

    let options = ["Search", "My mixes", "My playlists", "My library", "Local files", "Config", "Quit"];
    let mut cursor: usize = 0;

    loop {
        enable_raw_mode()?;
        let mut terminal = {
            let mut stdout = io::stdout();
            execute!(stdout, Clear(ClearType::All), EnterAlternateScreen)?;
            Terminal::new(CrosstermBackend::new(stdout))?
        };

        // Drain stale key events left over from sub-screens
        while event::poll(Duration::ZERO)? {
            let _ = event::read();
        }

        let choice = loop {
            terminal.draw(|f| {
                let area = f.area();
                let banner_h = BANNER_LINES.len() as u16;

                // Banner
                let banner_area = Rect::new(area.x, area.y, area.width, banner_h.min(area.height));
                f.render_widget(
                    Paragraph::new(
                        BANNER_LINES
                            .iter()
                            .map(|l| Line::from(Span::styled(format!("  {l}"), Style::default().fg(accent_color))))
                            .collect::<Vec<_>>(),
                    ),
                    banner_area,
                );

                // Menu
                if banner_h < area.height {
                    let menu_area = Rect::new(area.x, area.y + banner_h, area.width, area.height - banner_h);
                    let items: Vec<ListItem> = options
                        .iter()
                        .enumerate()
                        .map(|(i, label)| {
                            if i == cursor {
                                ListItem::new(format!("> {label}"))
                                    .style(Style::default().add_modifier(Modifier::BOLD))
                            } else {
                                ListItem::new(format!("  {label}"))
                            }
                        })
                        .collect();
                    f.render_widget(List::new(items), menu_area);
                }

                // Queue overlay
                if let Some(qs) = crate::DOWNLOAD_QUEUE.get().and_then(|q| q.status()) {
                    let qs_text = format!(" {} ", qs);
                    let w = qs_text.chars().count() as u16;
                    let qs_area = Rect::new(
                        area.width.saturating_sub(w),
                        area.height.saturating_sub(1),
                        w.min(area.width),
                        1,
                    );
                    f.render_widget(
                        Paragraph::new(qs_text).style(Style::default().add_modifier(Modifier::DIM)),
                        qs_area,
                    );
                }
            })?;

            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    match key.code {
                        KeyCode::Up | KeyCode::Char('k') => {
                            if cursor > 0 {
                                cursor -= 1;
                            }
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            if cursor + 1 < options.len() {
                                cursor += 1;
                            }
                        }
                        KeyCode::Enter => break Some(cursor),
                        KeyCode::Esc | KeyCode::Char('q') => break None,
                        _ => {}
                    }
                }
            }
        };

        let _ = disable_raw_mode();
        let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
        drop(terminal);

        match choice {
            None | Some(6) => return Ok(()),
            Some(0) => {
                let query: String = dialoguer::Input::new()
                    .with_prompt("Search")
                    .allow_empty(true)
                    .interact_text()?;
                if !query.trim().is_empty() {
                    let session = auth::get_session()?;
                    let mut client = api::TidalClient::new(session);
                    search::run(&mut client, &query, 10, false)?;
                }
            }
            Some(1) => {
                let session = auth::get_session()?;
                let mut client = api::TidalClient::new(session);
                mix::run(&mut client, false)?;
            }
            Some(2) => {
                let session = auth::get_session()?;
                let mut client = api::TidalClient::new(session);
                playlist::run(&mut client, false)?;
            }
            Some(3) => {
                let session = auth::get_session()?;
                let mut client = api::TidalClient::new(session);
                library::run(&mut client, false)?;
            }
            Some(4) => {
                match local::run(false)?.as_str() {
                    "mixes" => {
                        let session = auth::get_session()?;
                        let mut client = api::TidalClient::new(session);
                        mix::run(&mut client, false)?;
                    }
                    "search" => {
                        let query: String = dialoguer::Input::new()
                            .with_prompt("Search")
                            .allow_empty(true)
                            .interact_text()?;
                        if !query.trim().is_empty() {
                            let session = auth::get_session()?;
                            let mut client = api::TidalClient::new(session);
                            search::run(&mut client, &query, 10, false)?;
                        }
                    }
                    _ => {}
                }
            }
            Some(5) => {
                let cfg_options = ["Edit in app", "Open JSON file"];
                if let Some(c) = dialoguer::Select::new()
                    .items(&cfg_options)
                    .default(0)
                    .report(false)
                    .interact_opt()?
                {
                    match c {
                        0 => config::edit_interactive()?,
                        _ => config::open_editor()?,
                    }
                }
            }
            _ => {}
        }
    }
}
