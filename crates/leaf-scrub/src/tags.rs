//! mp3, FLAC, Ogg / Opus, WAV: the tags (ID3, Vorbis comments, APE, RIFF INFO) and the cover art, removed with lofty's writers,
//! which move the audio data as it is (no re-encoding). The result is read back and the audio properties must be unchanged.

use crate::{Finding, Inspection, Options, Result, ScrubError};
use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::probe::Probe;
use lofty::tag::{ItemValue, Tag, TagExt, TagType};
use std::path::Path;

fn read(path: &Path) -> Result<lofty::file::TaggedFile> {
    Probe::open(path)
        .map_err(|e| ScrubError::Malformed(format!("the file could not be read: {e}")))?
        .options(ParseOptions::new().read_cover_art(true))
        .guess_file_type()
        .map_err(ScrubError::Io)?
        .read()
        .map_err(|e| ScrubError::Malformed(format!("the audio could not be read: {e}")))
}

pub(crate) fn tag_findings(tags: &[Tag], keep_cover: bool) -> Vec<Finding> {
    let mut out = Vec::new();
    for t in tags {
        for item in t.items() {
            let value = match item.value() {
                ItemValue::Text(s) | ItemValue::Locator(s) => s.clone(),
                ItemValue::Binary(b) => format!("{} bytes", b.len()),
            };
            out.push(Finding::new(format!("{:?}", item.key()), value, true));
        }
        for p in t.pictures() {
            let mime = p.mime_type().map(|m| m.to_string()).unwrap_or_else(|| "picture".into());
            out.push(Finding::new("Cover art", format!("{mime}, {} bytes", p.data().len()), !keep_cover));
        }
    }
    out
}

pub(crate) fn inspect(path: &Path, opts: &Options) -> Result<Inspection> {
    let f = read(path)?;
    let findings = tag_findings(f.tags(), opts.keep_cover_art);
    Ok(Inspection {
        format: format!("{:?} audio", f.file_type()).replace(" audio", ""),
        findings,
        notes: vec!["Still inside: the encoder name some programs write into the audio stream itself is not removed.".into()],
        can_clean: true,
        refusal: None,
    })
}

pub(crate) fn produce(src: &Path, dst: &Path, opts: &Options) -> Result<()> {
    let before = read(src)?;
    let props = before.properties();
    let sig = (props.duration(), props.sample_rate(), props.channels());
    crate::copy_without_attrs(src, dst)?;
    let wo = WriteOptions::default();
    for t in before.tags() {
        let ty: TagType = t.tag_type();
        if opts.keep_cover_art && t.picture_count() > 0 {
            let mut keep = Tag::new(ty);
            for p in t.pictures() {
                keep.push_picture(p.clone());
            }
            keep.save_to_path(dst, wo).map_err(|e| ScrubError::Verify(format!("a tag could not be rewritten: {e}")))?;
        } else {
            ty.remove_from_path(dst, wo).map_err(|e| ScrubError::Verify(format!("a tag could not be removed: {e}")))?;
        }
    }
    let after = read(dst)?;
    let p = after.properties();
    if (p.duration(), p.sample_rate(), p.channels()) != sig {
        return Err(ScrubError::Verify("the audio changed while the tags were removed".into()));
    }
    Ok(())
}
