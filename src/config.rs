use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".config")
        .join("lumitide")
        .join("config.json")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_search_limit")]
    pub search_limit: u32,
    #[serde(default = "default_output_dir")]
    pub output_dir: String,
    #[serde(default = "default_cover_size")]
    pub cover_size: u32,
    #[serde(default = "default_volume")]
    pub volume: f32,
    #[serde(default = "default_true")]
    pub drop_detection: bool,
    #[serde(default)]
    pub always_color: bool,
    #[serde(default)]
    pub pywal: bool,
    #[serde(default)]
    pub calm_mode: bool,
    #[serde(default = "default_true")]
    pub show_controls_hint: bool,
}

fn default_search_limit() -> u32 { 10 }
fn default_output_dir() -> String { ".".to_string() }
fn default_cover_size() -> u32 { 640 }
fn default_volume() -> f32 { 0.5 }
fn default_true() -> bool { true }

impl Default for Config {
    fn default() -> Self {
        Self {
            search_limit:       10,
            output_dir:         ".".to_string(),
            cover_size:         640,
            volume:             0.5,
            drop_detection:     true,
            always_color:       true,
            pywal:              false,
            calm_mode:          false,
            show_controls_hint: true,
        }
    }
}

pub fn load() -> Config {
    let path = config_path();
    let mut cfg = Config::default();
    if path.exists() {
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Config>(&text) {
                Ok(loaded) => cfg = loaded,
                Err(e) => eprintln!("warning: config file is invalid and will be ignored: {e}"),
            },
            Err(e) => eprintln!("warning: could not read config file: {e}"),
        }
    }
    cfg.output_dir = expand_tilde(&cfg.output_dir);
    cfg
}

fn expand_tilde(path: &str) -> String {
    if path == "~" {
        return dirs::home_dir()
            .map(|h| h.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string());
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return dirs::home_dir()
            .map(|h| h.join(rest).to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string());
    }
    path.to_string()
}

pub fn save_volume(volume: f32) -> Result<()> {
    let mut cfg = load();
    cfg.volume = volume;
    save(&cfg)
}

pub fn save(cfg: &Config) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(cfg)?;
    std::fs::write(&path, text)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_values() {
        let cfg = Config::default();
        assert_eq!(cfg.search_limit, 10);
        assert_eq!(cfg.output_dir, ".");
        assert_eq!(cfg.cover_size, 640);
        assert!((cfg.volume - 0.5).abs() < f32::EPSILON);
        assert!(cfg.drop_detection);
        assert!(cfg.always_color);
        assert!(!cfg.calm_mode);
        assert!(cfg.show_controls_hint);
    }

    #[test]
    fn config_round_trips_through_json() {
        let mut cfg = Config::default();
        cfg.volume = 0.3;
        cfg.search_limit = 25;
        let json = serde_json::to_string(&cfg).unwrap();
        let loaded: Config = serde_json::from_str(&json).unwrap();
        assert!((loaded.volume - 0.3).abs() < 1e-6);
        assert_eq!(loaded.search_limit, 25);
    }

    #[test]
    fn config_partial_json_uses_defaults() {
        let json = r#"{"volume": 0.8}"#;
        let cfg: Config = serde_json::from_str(json).unwrap();
        assert!((cfg.volume - 0.8).abs() < 1e-6);
        assert_eq!(cfg.search_limit, 10);
        assert_eq!(cfg.output_dir, ".");
    }

    #[test]
    fn expand_tilde_resolves_bare_tilde_to_home() {
        let home = dirs::home_dir().expect("home dir available");
        assert_eq!(expand_tilde("~"), home.to_string_lossy());
    }

    #[test]
    fn expand_tilde_resolves_tilde_slash_path() {
        let home = dirs::home_dir().expect("home dir available");
        let expected = home.join("Downloads").join("DJ").to_string_lossy().into_owned();
        assert_eq!(expand_tilde("~/Downloads/DJ"), expected);
    }

    #[test]
    fn expand_tilde_leaves_other_paths_unchanged() {
        assert_eq!(expand_tilde("."), ".");
        assert_eq!(expand_tilde("/absolute/path"), "/absolute/path");
        assert_eq!(expand_tilde("relative/path"), "relative/path");
        assert_eq!(expand_tilde("/foo/~bar"), "/foo/~bar");
        assert_eq!(expand_tilde("~user"), "~user");
    }
}

pub fn edit_interactive() -> Result<()> {
    use dialoguer::{Confirm, Input, Select};
    use std::io::Write;

    loop {
        let mut cfg = load();

        // Build menu items showing current values
        let items = vec![
            format!("Download folder     {}", cfg.output_dir),
            format!("Search results      {}", cfg.search_limit),
            format!("Drop detection      {}", if cfg.drop_detection { "on" } else { "off" }),
            format!("Always color        {}", if cfg.always_color   { "on" } else { "off" }),
            format!("Pywal colors        {}", if cfg.pywal          { "on" } else { "off" }),
            format!("Calm mode           {}", if cfg.calm_mode      { "on" } else { "off" }),
            format!("Controls hint       {}", if cfg.show_controls_hint { "on" } else { "off" }),
            "Back".to_string(),
        ];

        print!("\x1B[2J\x1B[H");
        let _ = std::io::stdout().flush();

        let Some(choice) = Select::new()
            .with_prompt("Settings")
            .items(&items)
            .default(0)
            .report(false)
            .interact_opt()?
        else { break };

        match choice {
            0 => {
                #[cfg(windows)]
                {
                    let picked = rfd::FileDialog::new()
                        .set_directory(&cfg.output_dir)
                        .pick_folder();
                    if let Some(path) = picked {
                        cfg.output_dir = path.to_string_lossy().into_owned();
                        save(&cfg)?;
                    }
                }
                #[cfg(not(windows))]
                {
                    let input: String = Input::new()
                        .with_prompt("Download folder")
                        .with_initial_text(&cfg.output_dir)
                        .allow_empty(true)
                        .interact_text()?;
                    if !input.trim().is_empty() {
                        cfg.output_dir = input.trim().to_owned();
                        save(&cfg)?;
                    }
                }
            }
            1 => {
                let input: String = Input::new()
                    .with_prompt("Search results (1–50)")
                    .with_initial_text(&cfg.search_limit.to_string())
                    .allow_empty(true)
                    .interact_text()?;
                if let Ok(n) = input.trim().parse::<u32>() {
                    cfg.search_limit = n.clamp(1, 50);
                    save(&cfg)?;
                }
            }
            2 => { cfg.drop_detection     = Confirm::new().with_prompt("Drop detection").default(cfg.drop_detection).interact()?;     save(&cfg)?; }
            3 => { cfg.always_color       = Confirm::new().with_prompt("Always color").default(cfg.always_color).interact()?;         save(&cfg)?; }
            4 => { cfg.pywal              = Confirm::new().with_prompt("Pywal colors").default(cfg.pywal).interact()?;                save(&cfg)?; }
            5 => { cfg.calm_mode          = Confirm::new().with_prompt("Calm mode").default(cfg.calm_mode).interact()?;               save(&cfg)?; }
            6 => { cfg.show_controls_hint = Confirm::new().with_prompt("Controls hint").default(cfg.show_controls_hint).interact()?;  save(&cfg)?; }
            _ => break,
        }
    }
    Ok(())
}

pub fn open_editor() -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if !path.exists() {
        save(&Config::default())?;
    }
    // Windows: use start, Unix: use $EDITOR or xdg-open
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", &path.to_string_lossy().into_owned()])
            .spawn()?;
    }
    #[cfg(not(target_os = "windows"))]
    {
        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "xdg-open".to_string());
        std::process::Command::new(editor).arg(&path).spawn()?;
    }
    Ok(())
}
