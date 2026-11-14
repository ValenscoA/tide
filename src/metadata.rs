use anyhow::Result;
use metaflac::block::PictureType;
use metaflac::Tag;
use std::path::Path;

use crate::api::TrackInfo;

pub fn embed(path: &Path, track: &TrackInfo, cover_bytes: Option<&[u8]>) -> Result<()> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if ext == "m4a" || ext == "mp4" {
        embed_m4a(path, track, cover_bytes)
    } else if ext == "flac" {
        embed_flac(path, track, cover_bytes)
    } else {
        Ok(())
    }
}

fn embed_flac(path: &Path, track: &TrackInfo, cover_bytes: Option<&[u8]>) -> Result<()> {
    let mut tag = Tag::read_from_path(path).unwrap_or_else(|_| Tag::new());
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
        if let Some(ref isrc) = track.isrc {
            vc.set("ISRC", vec![isrc.clone()]);
        }
        if let Some(ref copy) = track.album_copyright {
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
        let (w, h) = image_dimensions(bytes);
        pic.width = w;
        pic.height = h;
        pic.depth = 24;
        pic.data = bytes.to_vec();
        tag.push_block(metaflac::Block::Picture(pic));
    }

    tag.save()?;
    Ok(())
}

fn embed_m4a(path: &Path, track: &TrackInfo, cover_bytes: Option<&[u8]>) -> Result<()> {
    use lofty::config::WriteOptions;
    use lofty::picture::{MimeType, Picture, PictureType};
    use lofty::prelude::{Accessor, AudioFile, TaggedFileExt};

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

    tagged
        .save_to_path(path, WriteOptions::default())
        .map_err(|e| anyhow::anyhow!("lofty save: {}", e))?;
    Ok(())
}

fn image_dimensions(bytes: &[u8]) -> (u32, u32) {
    match image::load_from_memory(bytes) {
        Ok(img) => (img.width(), img.height()),
        Err(_) => (640, 640),
    }
}

pub fn read_local_track(path: &Path) -> (TrackInfo, Option<Vec<u8>>) {
    use lofty::picture::PictureType;
    use lofty::prelude::*;

    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Unknown")
        .to_string();
    let fmt = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_uppercase())
        .unwrap_or_else(|| "AUDIO".to_string());

    let mut title = stem;
    let mut artist_name = "Local File".to_string();
    let mut album_name = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("Local")
        .to_string();
    let mut duration = 0u64;
    let mut cover_bytes = None;

    if let Ok(tagged) = lofty::read_from_path(path) {
        duration = tagged.properties().duration().as_secs();
        if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
            if let Some(t) = tag.title() {
                if !t.is_empty() {
                    title = t.into_owned();
                }
            }
            if let Some(a) = tag.artist() {
                if !a.is_empty() {
                    artist_name = a.into_owned();
                }
            }
            if let Some(al) = tag.album() {
                if !al.is_empty() {
                    album_name = al.into_owned();
                }
            }
            cover_bytes = tag
                .pictures()
                .iter()
                .find(|p| p.pic_type() == PictureType::CoverFront)
                .or_else(|| tag.pictures().first())
                .map(|p| p.data().to_vec());
        }
    }

    (
        TrackInfo {
            id: 0,
            title,
            artist_name: artist_name.clone(),
            artist_id: None,
            artists: vec![artist_name],
            album_name,
            album_id: None,
            album_cover: None,
            album_artist: String::new(),
            album_copyright: None,
            album_release_year: None,
            track_num: 1,
            volume_num: 1,
            isrc: None,
            audio_quality: fmt,
            duration,
            explicit: false,
            hires: false,
            local_path: Some(path.to_string_lossy().to_string()),
        },
        cover_bytes,
    )
}
