//! Strict typed timeline over already checked HLS resources. No network authority.
use super::network::{Fetched, HlsManifest, HlsResource, ResourceKind};
use reqwest::Url;
use std::{sync::Arc, time::Duration};

const MAX_BYTES: usize = 65_536;
const MAX_SEGMENTS: usize = 64;
const MAX_SECONDS: u64 = 30;

/// Closed diagnostics: never includes source text, URI, credentials or tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TimelineError {
    Invalid,
    Unsupported,
    TooLarge,
    Provenance,
    Overflow,
}

// Deliberately no Debug/Serialize: resources contain private checked addresses.
pub(super) struct MediaSegment {
    pub sequence: u64,
    pub duration: Duration,
    pub resource: HlsResource,
}
pub(super) struct MediaPlaylist {
    pub media_sequence: u64,
    pub discontinuity_sequence: u64,
    pub target_duration: Duration,
    pub end_list: bool,
    pub segments: Vec<MediaSegment>,
}

/// The caller must first use its PreparedSource::parse_hls for URI authorization.
/// These are MPEG-TS candidates: the runtime must still validate fetched segment
/// bytes through its explicit MPEG-TS reader before publication.
/// This consumes those exact opaque children; URI joining below is equality
/// binding only and never grants origin, DNS, credential or fetch authority.
pub(super) fn parse(
    fetched: &Fetched,
    manifest: HlsManifest,
) -> Result<MediaPlaylist, TimelineError> {
    parse_checked_parts(
        fetched.bytes(),
        &fetched.url,
        fetched.provenance.as_ref(),
        manifest,
    )
}

// Selected once; integration must retain this exact resource identity through
// Probe and Decode. This function does not implement ABR or recursive masters.
pub(super) struct SelectedVariant {
    pub bandwidth: u64,
    pub resource: HlsResource,
}
pub(super) fn select_master(
    fetched: &Fetched,
    manifest: HlsManifest,
    pinned: Option<&HlsResource>,
) -> Result<SelectedVariant, TimelineError> {
    master_checked_parts(
        fetched.bytes(),
        &fetched.url,
        fetched.provenance.as_ref(),
        manifest,
        pinned,
    )
}
fn attributes(mut text: &str) -> Result<Vec<(&str, &str)>, TimelineError> {
    let mut fields = Vec::new();
    loop {
        let (key, tail) = text.split_once('=').ok_or(TimelineError::Invalid)?;
        if key.is_empty()
            || !key.bytes().all(|b| b.is_ascii_uppercase() || b == b'-')
            || fields.iter().any(|(seen, _)| *seen == key)
        {
            return Err(TimelineError::Invalid);
        }
        let (value, rest) = if let Some(quoted) = tail.strip_prefix('"') {
            let end = quoted.find('"').ok_or(TimelineError::Invalid)?;
            (&quoted[..end], &quoted[end + 1..])
        } else {
            tail.find(',')
                .map_or((tail, ""), |at| (&tail[..at], &tail[at..]))
        };
        if value.is_empty() {
            return Err(TimelineError::Invalid);
        }
        fields.push((key, value));
        if fields.len() > 8 {
            return Err(TimelineError::TooLarge);
        }
        if rest.is_empty() {
            return Ok(fields);
        }
        text = rest
            .strip_prefix(',')
            .filter(|s| !s.is_empty())
            .ok_or(TimelineError::Invalid)?;
    }
}
fn variant_bandwidth(text: &str) -> Result<u64, TimelineError> {
    let mut bandwidth = None;
    for (key, value) in attributes(text)? {
        match key {
            "BANDWIDTH" => {
                let value = number(value)?;
                if value == 0 || value > 100_000_000 {
                    return Err(TimelineError::Unsupported);
                }
                bandwidth = Some(value);
            }
            "AVERAGE-BANDWIDTH" => {
                if !(1..=100_000_000).contains(&number(value)?) {
                    return Err(TimelineError::Unsupported);
                }
            }
            "RESOLUTION" => {
                let (w, h) = value.split_once('x').ok_or(TimelineError::Invalid)?;
                if !(1..=8192).contains(&number(w)?) || !(1..=8192).contains(&number(h)?) {
                    return Err(TimelineError::Unsupported);
                }
            }
            "FRAME-RATE" => {
                let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
                let whole = number(whole)?;
                if !(1..=240).contains(&whole)
                    || fraction.len() > 3
                    || !fraction.bytes().all(|b| b.is_ascii_digit())
                    || (value.contains('.') && fraction.is_empty())
                    || (whole == 240 && fraction.bytes().any(|b| b != b'0'))
                {
                    return Err(TimelineError::Unsupported);
                }
            }
            "CODECS" => {
                if value.len() > 256
                    || value.split(',').any(|codec| {
                        codec.is_empty()
                            || !codec
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
                    })
                {
                    return Err(TimelineError::Unsupported);
                }
            }
            "CLOSED-CAPTIONS" if value == "NONE" => {}
            // No external rendition group, URI attribute or unknown extension.
            _ => return Err(TimelineError::Unsupported),
        }
    }
    bandwidth.ok_or(TimelineError::Invalid)
}
fn master_checked_parts(
    bytes: &[u8],
    base: &Url,
    provenance: Option<&Arc<()>>,
    manifest: HlsManifest,
    pinned: Option<&HlsResource>,
) -> Result<SelectedVariant, TimelineError> {
    if bytes.len() > MAX_BYTES || manifest.resources.len() > 8 {
        return Err(TimelineError::TooLarge);
    }
    let provenance = provenance.ok_or(TimelineError::Provenance)?;
    if manifest
        .resources
        .iter()
        .any(|r| r.kind != ResourceKind::Playlist)
    {
        return Err(TimelineError::Unsupported);
    }
    if manifest.resources.iter().any(|r| {
        !r.provenance
            .as_ref()
            .is_some_and(|p| Arc::ptr_eq(p, provenance))
    }) {
        return Err(TimelineError::Provenance);
    }
    if manifest.ended {
        return Err(TimelineError::Invalid);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| TimelineError::Invalid)?;
    let mut lines = text.lines();
    if lines.next() != Some("#EXTM3U") {
        return Err(TimelineError::Invalid);
    }
    let mut children = manifest.resources.into_iter();
    let mut pending = None;
    let mut variants: Vec<SelectedVariant> = Vec::new();
    let mut version = false;
    let mut independent = false;
    for raw in lines {
        let line = raw.trim();
        if line.is_empty() || (line.starts_with('#') && !line.starts_with("#EXT")) {
            continue;
        }
        if line.len() > 8192 {
            return Err(TimelineError::TooLarge);
        }
        if line.contains("{$") {
            return Err(TimelineError::Unsupported);
        }
        if let Some(attrs) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            if pending.is_some() {
                return Err(TimelineError::Invalid);
            }
            pending = Some(variant_bandwidth(attrs)?);
        } else if let Some(value) = line.strip_prefix("#EXT-X-VERSION:") {
            if version || pending.is_some() {
                return Err(TimelineError::Invalid);
            }
            if !(1..=7).contains(&number(value)?) {
                return Err(TimelineError::Unsupported);
            }
            version = true;
        } else if line == "#EXT-X-INDEPENDENT-SEGMENTS" {
            if independent || pending.is_some() {
                return Err(TimelineError::Invalid);
            }
            independent = true;
        } else if line.starts_with('#') {
            return Err(TimelineError::Unsupported);
        } else {
            let bandwidth = pending.take().ok_or(TimelineError::Invalid)?;
            let resource = children.next().ok_or(TimelineError::Provenance)?;
            if base.join(line).map_err(|_| TimelineError::Invalid)? != resource.url {
                return Err(TimelineError::Provenance);
            }
            if base.origin() != resource.url.origin() {
                return Err(TimelineError::Provenance);
            }
            if variants.iter().any(|old| old.resource.url == resource.url) {
                return Err(TimelineError::Unsupported);
            }
            variants.push(SelectedVariant {
                bandwidth,
                resource,
            });
        }
    }
    if pending.is_some() || variants.is_empty() || children.next().is_some() {
        return Err(TimelineError::Invalid);
    }
    if let Some(pinned) = pinned {
        if pinned.kind != ResourceKind::Playlist
            || !pinned
                .provenance
                .as_ref()
                .is_some_and(|p| Arc::ptr_eq(p, provenance))
            || base.origin() != pinned.url.origin()
        {
            return Err(TimelineError::Provenance);
        }
        let index = variants
            .iter()
            .position(|candidate| candidate.resource.url == pinned.url)
            .ok_or(TimelineError::Provenance)?;
        return Ok(variants.swap_remove(index));
    }
    variants.sort_by(|a, b| {
        a.bandwidth
            .cmp(&b.bandwidth)
            .then_with(|| a.resource.url.as_str().cmp(b.resource.url.as_str()))
    });
    Ok(variants.remove(0))
}

fn number(text: &str) -> Result<u64, TimelineError> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(TimelineError::Invalid);
    }
    text.parse().map_err(|_| TimelineError::Overflow)
}
fn duration(text: &str) -> Result<Duration, TimelineError> {
    let (seconds, fraction) = text.split_once('.').unwrap_or((text, ""));
    let seconds = number(seconds)?;
    if fraction.len() > 9
        || !fraction.bytes().all(|b| b.is_ascii_digit())
        || (text.contains('.') && fraction.is_empty())
    {
        return Err(TimelineError::Invalid);
    }
    let nanos = if fraction.is_empty() {
        0
    } else {
        number(fraction)?
            .checked_mul(10u64.pow(9 - fraction.len() as u32))
            .ok_or(TimelineError::Overflow)?
    };
    let value = Duration::new(seconds, nanos as u32);
    if value.is_zero() || value > Duration::from_secs(MAX_SECONDS) {
        return Err(TimelineError::Unsupported);
    }
    Ok(value)
}

fn parse_checked_parts(
    bytes: &[u8],
    base: &Url,
    provenance: Option<&Arc<()>>,
    manifest: HlsManifest,
) -> Result<MediaPlaylist, TimelineError> {
    if bytes.len() > MAX_BYTES || manifest.resources.len() > MAX_SEGMENTS {
        return Err(TimelineError::TooLarge);
    }
    let provenance = provenance.ok_or(TimelineError::Provenance)?;
    if manifest
        .resources
        .iter()
        .any(|resource| resource.kind != ResourceKind::Segment)
    {
        return Err(TimelineError::Unsupported);
    }
    if manifest.resources.iter().any(|resource| {
        !resource
            .provenance
            .as_ref()
            .is_some_and(|p| Arc::ptr_eq(p, provenance))
    }) {
        return Err(TimelineError::Provenance);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| TimelineError::Invalid)?;
    let mut lines = text.lines();
    if lines.next() != Some("#EXTM3U") {
        return Err(TimelineError::Invalid);
    }
    let mut children = manifest.resources.into_iter();
    let mut target = None;
    let mut media = None;
    let mut discontinuity = None;
    let mut version = None;
    let mut playlist_type = None;
    let mut independent = false;
    let mut pending = None;
    let mut ended = false;
    let mut segments: Vec<MediaSegment> = Vec::new();
    for raw in lines {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line.len() > 8192 {
            return Err(TimelineError::TooLarge);
        }
        if line.contains("{$") {
            return Err(TimelineError::Unsupported);
        }
        if line.starts_with('#') && !line.starts_with("#EXT") {
            continue;
        }
        if ended {
            return Err(TimelineError::Invalid);
        }
        if line == "#EXT-X-ENDLIST" {
            if pending.is_some() {
                return Err(TimelineError::Invalid);
            }
            ended = true;
        } else if line == "#EXT-X-INDEPENDENT-SEGMENTS" {
            if independent {
                return Err(TimelineError::Invalid);
            }
            independent = true;
        } else if line == "#EXT-X-KEY:METHOD=NONE" {
            // Exact unencrypted reset only: no URI or other encryption attribute.
        } else if let Some(value) = line.strip_prefix("#EXT-X-VERSION:") {
            if version.replace(number(value)?).is_some() {
                return Err(TimelineError::Invalid);
            }
            if !matches!(version, Some(1..=7)) {
                return Err(TimelineError::Unsupported);
            }
        } else if let Some(value) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            if target.is_some() || !segments.is_empty() || pending.is_some() {
                return Err(TimelineError::Invalid);
            }
            let seconds = number(value)?;
            if !(1..=MAX_SECONDS).contains(&seconds) {
                return Err(TimelineError::Unsupported);
            }
            target = Some(Duration::from_secs(seconds));
        } else if let Some(value) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            if media.is_some() || !segments.is_empty() || pending.is_some() {
                return Err(TimelineError::Invalid);
            }
            media = Some(number(value)?);
        } else if let Some(value) = line.strip_prefix("#EXT-X-DISCONTINUITY-SEQUENCE:") {
            if discontinuity.is_some() || !segments.is_empty() || pending.is_some() {
                return Err(TimelineError::Invalid);
            }
            discontinuity = Some(number(value)?);
        } else if let Some(value) = line.strip_prefix("#EXT-X-PLAYLIST-TYPE:") {
            if playlist_type.replace(value).is_some() || !segments.is_empty() || pending.is_some() {
                return Err(TimelineError::Invalid);
            }
            if !matches!(value, "EVENT" | "VOD") {
                return Err(TimelineError::Unsupported);
            }
        } else if let Some(value) = line.strip_prefix("#EXTINF:") {
            if pending.is_some() {
                return Err(TimelineError::Invalid);
            }
            let (value, _) = value.split_once(',').ok_or(TimelineError::Invalid)?;
            pending = Some(duration(value)?);
        } else if line.starts_with('#') {
            // Includes master playlists, MAP/fMP4, encryption, range, LL-HLS,
            // GAP, DISCONTINUITY and all unknown extension tags.
            return Err(TimelineError::Unsupported);
        } else {
            let duration = pending.take().ok_or(TimelineError::Invalid)?;
            let target = target.ok_or(TimelineError::Invalid)?;
            // HLS target duration constrains EXTINF rounded to nearest second.
            if (duration.as_nanos() + 500_000_000) / 1_000_000_000 > u128::from(target.as_secs()) {
                return Err(TimelineError::Invalid);
            }
            let resource = children.next().ok_or(TimelineError::Provenance)?;
            let expected = base.join(line).map_err(|_| TimelineError::Invalid)?;
            if expected != resource.url {
                return Err(TimelineError::Provenance);
            }
            if matches!(
                resource
                    .url
                    .path()
                    .rsplit('.')
                    .next()
                    .map(str::to_ascii_lowercase)
                    .as_deref(),
                Some("mp4" | "m4s" | "aac" | "vtt")
            ) {
                return Err(TimelineError::Unsupported);
            }
            if segments.iter().any(|old| old.resource.url == resource.url) {
                return Err(TimelineError::Unsupported);
            }
            let sequence = media
                .unwrap_or(0)
                .checked_add(segments.len() as u64)
                .ok_or(TimelineError::Overflow)?;
            // Reserve a representable next-sequence cursor; never wrap at MAX.
            sequence.checked_add(1).ok_or(TimelineError::Overflow)?;
            segments.push(MediaSegment {
                sequence,
                duration,
                resource,
            });
        }
    }
    if pending.is_some()
        || segments.is_empty()
        || children.next().is_some()
        || ended != manifest.ended
        || (playlist_type == Some("VOD") && !ended)
    {
        return Err(TimelineError::Invalid);
    }
    Ok(MediaPlaylist {
        media_sequence: media.unwrap_or(0),
        discontinuity_sequence: discontinuity.unwrap_or(0),
        target_duration: target.ok_or(TimelineError::Invalid)?,
        end_list: ended,
        segments,
    })
}

#[cfg(test)]
#[path = "hls_timeline_tests.rs"]
mod tests;
