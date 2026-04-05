mod api;
mod auth;
mod color_state;
mod config;
mod cover;
mod local;
mod metadata;
mod mix;
mod panel;
mod preview;
mod search;
mod spectrum;
mod utils;

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
    /// Shuffle and play local audio files (FLAC, MP3, M4A)
    Local {
        #[arg(long, hide = true)]
        debug: bool,
    },
    /// Open the config file in your default editor
    Config,
}

fn main() -> Result<()> {
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

        Some(Commands::Local { debug }) => local::run(debug).map(|_| ()),

        Some(Commands::Config) => config::open_editor(),
    }
}

fn interactive_menu() -> Result<()> {
    use dialoguer::{Input, Select};

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
            if let Some(0) = Select::new()
                .items(&options)
                .default(0)
                .report(false)
                .interact_opt()?
            {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    let mut cfg = cfg;
                    cfg.output_dir = path.to_string_lossy().into_owned();
                    config::save(&cfg)?;
                }
            }
        }
    }

    let options = [
        "Search",
        "My mixes",
        "Local files",
        "Config",
        "Quit",
    ];

    loop {
        // Clear the terminal so previous output doesn't accumulate above the menu
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
            0 => {
                let query: String = Input::new()
                    .with_prompt("Search")
                    .allow_empty(true)
                    .interact_text()?;
                if !query.trim().is_empty() {
                    let session = auth::get_session()?;
                    let mut client = api::TidalClient::new(session);
                    search::run(&mut client, &query, 10, false)?;
                }
            }
            1 => {
                let session = auth::get_session()?;
                let mut client = api::TidalClient::new(session);
                mix::run(&mut client, false)?;
            }
            2 => {
                match local::run(false)?.as_str() {
                    "mixes" => {
                        let session = auth::get_session()?;
                        let mut client = api::TidalClient::new(session);
                        mix::run(&mut client, false)?;
                    }
                    "search" => {
                        let query: String = Input::new()
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
            3 => {
                print!("\x1B[2J\x1B[H");
                use std::io::Write;
                let _ = std::io::stdout().flush();
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
            _ => return Ok(()),
        }
    }
}
