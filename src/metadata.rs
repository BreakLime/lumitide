use anyhow::Result;
use metaflac::Tag;
use metaflac::block::PictureType;
use std::path::Path;

use crate::api::TrackInfo;

/// Embed Vorbis tags and cover art into a FLAC file.
pub fn embed(path: &Path, track: &TrackInfo, cover_bytes: Option<&[u8]>) -> Result<()> {
    let mut tag = Tag::read_from_path(path)
        .unwrap_or_else(|_| Tag::new());

    // Clear existing tags and pictures
    tag.remove_blocks(metaflac::block::BlockType::VorbisComment);
    tag.remove_blocks(metaflac::block::BlockType::Picture);

    // Vorbis comments
    {
        let vc = tag.vorbis_comments_mut();
        vc.set_title(vec![track.title.clone()]);
        vc.set_artist(if track.artists.is_empty() {
            vec![track.artist_name.clone()]
        } else {
            track.artists.clone()
        });
        vc.set_album(vec![track.album_name.clone()]);
        vc.set("ALBUMARTIST", vec![track.album_artist.clone()]);
        vc.set("TRACKNUMBER", vec![track.track_num.to_string()]);
        vc.set("DISCNUMBER", vec![track.volume_num.to_string()]);
        if let Some(isrc) = &track.isrc {
            vc.set("ISRC", vec![isrc.clone()]);
        }
        if let Some(copy) = &track.album_copyright {
            vc.set("COPYRIGHT", vec![copy.clone()]);
        }
        if let Some(year) = track.album_release_year {
            vc.set("DATE", vec![year.to_string()]);
        }
    }

    // Embed cover art
    if let Some(bytes) = cover_bytes {
        let mut pic = metaflac::block::Picture::new();
        pic.picture_type = PictureType::CoverFront;
        pic.mime_type = "image/jpeg".to_string();
        pic.description = String::new();
        // Attempt to read dimensions from the JPEG
        let (w, h) = image_dimensions(bytes);
        pic.width = w;
        pic.height = h;
        pic.depth = 24;
        pic.num_colors = 0;
        pic.data = bytes.to_vec();
        tag.push_block(metaflac::Block::Picture(pic));
    }

    tag.save()?;
    Ok(())
}

fn image_dimensions(bytes: &[u8]) -> (u32, u32) {
    let result = std::panic::catch_unwind(|| image::load_from_memory(bytes));
    match result {
        Ok(Ok(img)) => (img.width(), img.height()),
        _ => (640, 640),
    }
}
