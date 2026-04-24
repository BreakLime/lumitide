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

    #[test]
    fn audio_extension_flac() {
        assert_eq!(audio_extension(b"fLaC\x00\x00\x00\x22"), "flac");
    }

    #[test]
    fn audio_extension_m4a() {
        // MP4: 4-byte box size then "ftyp"
        assert_eq!(audio_extension(b"\x00\x00\x00\x1cftypisom"), "m4a");
    }

    #[test]
    fn audio_extension_fallback() {
        assert_eq!(audio_extension(b"\x00\x00\x00\x00"), "flac");
    }

    #[test]
    fn is_saved_finds_flac() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().to_string_lossy().to_string();
        std::fs::File::create(dir.path().join(safe_filename("Artist - Track.flac"))).unwrap();
        assert!(is_saved(&out, "Artist", "Track"));
    }

    #[test]
    fn is_saved_finds_m4a() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().to_string_lossy().to_string();
        std::fs::File::create(dir.path().join(safe_filename("Artist - Track.m4a"))).unwrap();
        assert!(is_saved(&out, "Artist", "Track"));
    }

    #[test]
    fn is_saved_returns_false_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().to_string_lossy().to_string();
        assert!(!is_saved(&out, "Artist", "Missing Track"));
    }
}

/// Detect audio file extension from the first 12 bytes of the file.
/// Returns `"flac"` for FLAC, `"m4a"` for MP4/AAC, or `"flac"` as a fallback.
pub fn audio_extension(magic: &[u8]) -> &'static str {
    if magic.starts_with(b"fLaC") {
        "flac"
    } else if magic.len() >= 8 && &magic[4..8] == b"ftyp" {
        "m4a"
    } else {
        "flac"
    }
}

/// Check if a track is already saved in the output directory (checks .flac and .m4a).
pub fn is_saved(out_dir: &str, artist: &str, title: &str) -> bool {
    let dir = PathBuf::from(out_dir);
    for ext in &["flac", "m4a"] {
        let exact = safe_filename(&format!("{} - {}.{}", artist, title, ext));
        if dir.join(&exact).exists() {
            return true;
        }
    }
    let title_lower = safe_filename(title).to_lowercase();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let fname = entry.file_name();
            let s = fname.to_string_lossy().to_lowercase();
            for ext in &[".flac", ".m4a"] {
                if s.ends_with(ext) {
                    let stem = &s[..s.len() - ext.len()];
                    if stem.contains(&title_lower) {
                        return true;
                    }
                }
            }
        }
    }
    false
}
