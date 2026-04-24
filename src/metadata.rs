use anyhow::Result;
use metaflac::Tag;
use metaflac::block::PictureType;
use std::path::Path;

use crate::api::TrackInfo;

/// Embed tags and cover art into an audio file (.flac or .m4a).
pub fn embed(path: &Path, track: &TrackInfo, cover_bytes: Option<&[u8]>) -> Result<()> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    if ext == "m4a" {
        embed_m4a(path, track, cover_bytes)
    } else {
        embed_flac(path, track, cover_bytes)
    }
}

fn embed_flac(path: &Path, track: &TrackInfo, cover_bytes: Option<&[u8]>) -> Result<()> {
    let mut tag = Tag::read_from_path(path)
        .unwrap_or_else(|_| Tag::new());

    tag.remove_blocks(metaflac::block::BlockType::VorbisComment);
    tag.remove_blocks(metaflac::block::BlockType::Picture);

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

    if let Some(bytes) = cover_bytes {
        let mut pic = metaflac::block::Picture::new();
        pic.picture_type = PictureType::CoverFront;
        pic.mime_type = "image/jpeg".to_string();
        pic.description = String::new();
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

fn embed_m4a(path: &Path, track: &TrackInfo, cover_bytes: Option<&[u8]>) -> Result<()> {
    use lofty::prelude::{Accessor, AudioFile, TaggedFileExt};
    use lofty::config::WriteOptions;
    use lofty::picture::{MimeType, Picture, PictureType};

    let mut tagged = lofty::read_from_path(path)
        .map_err(|e| anyhow::anyhow!("lofty open: {}", e))?;

    if let Some(tag) = tagged.primary_tag_mut() {
        tag.remove_picture_type(PictureType::CoverFront);
        tag.set_title(track.title.clone());
        tag.set_artist(track.artist_name.clone());
        tag.set_album(track.album_name.clone());
        tag.set_track(track.track_num);
        tag.set_disk(track.volume_num);

        if let Some(bytes) = cover_bytes {
            tag.push_picture(Picture::new_unchecked(
                PictureType::CoverFront,
                Some(MimeType::Jpeg),
                None,
                bytes.to_vec(),
            ));
        }
    }

    tagged.save_to_path(path, WriteOptions::default())
        .map_err(|e| anyhow::anyhow!("lofty save: {}", e))?;
    Ok(())
}

fn image_dimensions(bytes: &[u8]) -> (u32, u32) {
    let result = std::panic::catch_unwind(|| image::load_from_memory(bytes));
    match result {
        Ok(Ok(img)) => (img.width(), img.height()),
        _ => (640, 640),
    }
}
