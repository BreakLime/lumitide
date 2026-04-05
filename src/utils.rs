use std::path::PathBuf;

/// Remove filesystem-unsafe characters and truncate to 200 chars.
pub fn safe_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if r#"\/*?:"<>|"#.contains(c) { '_' } else { c })
        .collect();
    let trimmed = cleaned.trim_matches(|c| c == '.' || c == ' ');
    trimmed.chars().take(200).collect()
}

/// Format seconds as M:SS.
pub fn fmt_time(seconds: f64) -> String {
    let s = seconds as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_time_zero() {
        assert_eq!(fmt_time(0.0), "0:00");
    }

    #[test]
    fn fmt_time_under_one_minute() {
        assert_eq!(fmt_time(45.0), "0:45");
    }

    #[test]
    fn fmt_time_exact_minute() {
        assert_eq!(fmt_time(60.0), "1:00");
    }

    #[test]
    fn fmt_time_over_one_minute() {
        assert_eq!(fmt_time(185.0), "3:05");
    }

    #[test]
    fn safe_filename_replaces_unsafe_chars() {
        assert_eq!(safe_filename("a/b\\c*d?e:f"), "a_b_c_d_e_f");
    }

    #[test]
    fn safe_filename_trims_leading_trailing_dots_and_spaces() {
        assert_eq!(safe_filename("  .hello. "), "hello");
    }

    #[test]
    fn safe_filename_truncates_at_200_chars() {
        let long = "a".repeat(300);
        assert_eq!(safe_filename(&long).len(), 200);
    }

    #[test]
    fn safe_filename_normal_input_unchanged() {
        assert_eq!(safe_filename("Artist - Title"), "Artist - Title");
    }
}

/// Check if a FLAC track is already saved in the output directory.
pub fn is_saved(out_dir: &str, artist: &str, title: &str) -> bool {
    let dir = PathBuf::from(out_dir);
    let exact = safe_filename(&format!("{} - {}.flac", artist, title));
    if dir.join(&exact).exists() {
        return true;
    }
    // Fuzzy check: title anywhere in filename
    let title_lower = safe_filename(title).to_lowercase();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let fname = entry.file_name();
            let s = fname.to_string_lossy().to_lowercase();
            if s.ends_with(".flac") {
                let stem = s.trim_end_matches(".flac");
                if stem.contains(&title_lower) {
                    return true;
                }
            }
        }
    }
    false
}
