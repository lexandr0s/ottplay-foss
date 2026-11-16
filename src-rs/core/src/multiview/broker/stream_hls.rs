//! Explicit live HLS MPEG-TS input. Playlists remain in the checked broker;
//! FFmpeg receives only validated TS bytes through the existing owned socket.
//! No URL, key, manifest or provider credential crosses the decoder boundary.
use super::{
    hls_timeline::{self, MediaPlaylist, MediaSegment, TimelineError},
    network::{Error, Fetched, HlsResource, ResourceKind},
    resolvers::HlsSelection,
    runtime::{AuthorizedSource, RuntimeError},
    stream_transport::{check_packets, StreamLimits, CHUNK_BYTES},
};
use axum::body::Bytes;
use std::{
    collections::VecDeque,
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

const PACKET_BYTES: usize = 188;
const MAX_SEGMENT_BYTES: usize = 8 * 1024 * 1024;
const MAX_WINDOW: usize = 128;
const INITIAL_LIVE_SEGMENTS: usize = 3;
const MAX_PROGRESS_WAIT: Duration = Duration::from_secs(90);

// URI identity stays private, bounded by the checked <=64KiB playlist. It is
// used only for overlap/rewrite checks, never formatted into an error or receipt.
struct Identity {
    sequence: u64,
    uri: String,
    duration: Duration,
}
struct Cursor {
    first: u64,
    last: u64,
    discontinuity: u64,
    target: Duration,
    ended: bool,
    next: u64,
    prior: Vec<Identity>,
    pending: VecDeque<MediaSegment>,
}
impl Cursor {
    fn identities(playlist: &MediaPlaylist) -> Result<Vec<Identity>, Error> {
        if playlist.segments.is_empty() || playlist.segments.len() > MAX_WINDOW {
            return Err(Error::InvalidManifest);
        }
        playlist
            .segments
            .iter()
            .enumerate()
            .map(|(index, segment)| {
                if playlist.media_sequence.checked_add(index as u64) != Some(segment.sequence) {
                    return Err(Error::InvalidManifest);
                }
                Ok(Identity {
                    sequence: segment.sequence,
                    uri: segment.resource.url.as_str().into(),
                    duration: segment.duration,
                })
            })
            .collect()
    }
    fn new(mut playlist: MediaPlaylist) -> Result<Self, Error> {
        let prior = Self::identities(&playlist)?;
        let first = playlist.media_sequence;
        let last = prior.last().ok_or(Error::InvalidManifest)?.sequence;
        // Live starts near the edge; finite ENDLIST starts at its first segment.
        // This selects the starting sequence, not a burst of prefetched bodies.
        let skip = if playlist.end_list {
            0
        } else {
            playlist
                .segments
                .len()
                .saturating_sub(INITIAL_LIVE_SEGMENTS)
        };
        let pending: VecDeque<_> = playlist.segments.drain(skip..).collect();
        let next = pending.front().ok_or(Error::InvalidManifest)?.sequence;
        Ok(Self {
            first,
            last,
            discontinuity: playlist.discontinuity_sequence,
            target: playlist.target_duration,
            ended: playlist.end_list,
            next,
            prior,
            pending,
        })
    }
    fn reload(&mut self, playlist: MediaPlaylist) -> Result<bool, Error> {
        if !self.pending.is_empty() {
            return Err(Error::Busy);
        }
        let prior = Self::identities(&playlist)?;
        let last = prior.last().ok_or(Error::InvalidManifest)?.sequence;
        if playlist.media_sequence < self.first
            || last < self.last
            || playlist.media_sequence > self.next
            || playlist.discontinuity_sequence != self.discontinuity
            || playlist.target_duration != self.target
            || self.ended && (!playlist.end_list || last != self.last)
        {
            return Err(Error::InvalidManifest);
        }
        for old in &self.prior {
            if let Some(new) = prior.iter().find(|new| new.sequence == old.sequence) {
                if new.uri != old.uri || new.duration != old.duration {
                    return Err(Error::InvalidManifest);
                }
            }
        }
        let progressed = last > self.last;
        self.first = playlist.media_sequence;
        self.last = last;
        self.ended = playlist.end_list;
        self.prior = prior;
        self.pending = playlist
            .segments
            .into_iter()
            .filter(|segment| segment.sequence >= self.next)
            .collect();
        if self
            .pending
            .front()
            .is_some_and(|segment| segment.sequence != self.next)
        {
            return Err(Error::InvalidManifest);
        }
        Ok(progressed)
    }
    fn take(&mut self) -> Result<Option<MediaSegment>, Error> {
        let Some(segment) = self.pending.pop_front() else {
            return Ok(None);
        };
        if segment.sequence != self.next {
            return Err(Error::InvalidManifest);
        }
        self.next = self.next.checked_add(1).ok_or(Error::TooLarge)?;
        Ok(Some(segment))
    }
}

struct Segment {
    fetched: Fetched,
    offset: usize,
    duration: Duration,
    started: bool,
}
#[derive(Clone, Copy)]
struct ConsumerHold {
    started: Instant,
    released: Instant,
}
impl ConsumerHold {
    fn exclude_from(self, deadline: Instant) -> Result<Instant, Error> {
        // A pause cannot revive progress that had already expired at handoff.
        if deadline <= self.started {
            return Ok(deadline);
        }
        let elapsed = self
            .released
            .checked_duration_since(self.started)
            .ok_or(Error::InvalidResponse)?;
        deadline.checked_add(elapsed).ok_or(Error::Timeout)
    }
}

struct ChunkOwner {
    handed_off: Instant,
    released: Arc<Mutex<Option<ConsumerHold>>>,
    bytes: Bytes,
    // Retained slices keep source and per-source stream custody after Drop.
    // Factory::finish must report Busy until the last slice is released.
    _source: Arc<AuthorizedSource>,
    _stream: Arc<OwnedSemaphorePermit>,
    _chunk: OwnedSemaphorePermit,
}
impl AsRef<[u8]> for ChunkOwner {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

impl Drop for ChunkOwner {
    fn drop(&mut self) {
        // Bytes retains this owner until its LAST clone/slice is dropped. Record
        // release before the chunk permit is released; later polling is not hold time.
        *self
            .released
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(ConsumerHold {
            started: self.handed_off,
            released: Instant::now(),
        });
    }
}

pub(super) struct HlsTsStream {
    source: Arc<AuthorizedSource>,
    stream: Arc<OwnedSemaphorePermit>,
    cancel: CancellationToken,
    limits: StreamLimits,
    // None means a direct media playlist. A master is selected once per source,
    // pinned across Probe/Decode and never reselected during media reloads.
    playlist: Option<Arc<HlsResource>>,
    cursor: Cursor,
    segment: Option<Segment>,
    chunks: Arc<Semaphore>,
    released: Arc<Mutex<Option<ConsumerHold>>>,
    next_due: Instant,
    progress_deadline: Instant,
    failure: Option<Error>,
}
impl HlsTsStream {
    pub(super) async fn open(
        source: Arc<AuthorizedSource>,
        stream: Arc<OwnedSemaphorePermit>,
        cancel: CancellationToken,
        limits: StreamLimits,
    ) -> Result<Self, Error> {
        limits.validate()?;
        let deadline = (Instant::now() + limits.first_response).min(limits.hard_end);
        let root = checked(&source, &cancel, deadline, source.fetch_root()).await?;
        let manifest = source.parse_hls(&root).map_err(network_error)?;
        let pinned = source.hls_selection().map_err(network_error)?;
        let (playlist, media) = if manifest
            .resources
            .iter()
            .any(|r| r.kind == ResourceKind::Playlist)
        {
            let pin = match &pinned {
                Some(HlsSelection::Media) => return Err(Error::Unsupported),
                Some(HlsSelection::Variant(resource)) => Some(resource.as_ref()),
                None => None,
            };
            let selected =
                hls_timeline::select_master(&root, manifest, pin).map_err(timeline_error)?;
            let HlsSelection::Variant(resource) = source
                .pin_hls_selection(HlsSelection::Variant(Arc::new(selected.resource)))
                .map_err(network_error)?
            else {
                return Err(Error::InvalidManifest);
            };
            drop(root);
            let fetched = checked(
                &source,
                &cancel,
                deadline,
                source.fetch_hls_resource(&resource),
            )
            .await?;
            let manifest = source.parse_hls(&fetched).map_err(network_error)?;
            let media = parse_media(&fetched, manifest)?;
            (Some(resource), media)
        } else {
            source
                .pin_hls_selection(HlsSelection::Media)
                .map_err(network_error)?;
            (None, parse_media(&root, manifest)?)
        };
        let now = Instant::now();
        let cursor = Cursor::new(media)?;
        let progress_deadline = now + progress_wait(cursor.target, limits.idle);
        let mut result = Self {
            source,
            stream,
            cancel,
            limits,
            playlist,
            cursor,
            segment: None,
            chunks: Arc::new(Semaphore::new(1)),
            released: Arc::new(Mutex::new(None)),
            next_due: now,
            progress_deadline,
            failure: None,
        };
        // Bound the complete initial manifest/master/first segment by the same
        // first-response deadline. Unsupported bytes never reach the producer.
        result.load_segment(deadline).await?;
        Ok(result)
    }
    pub(super) fn cancel(&self) {
        self.cancel.cancel();
    }
    pub(super) fn cancellation(&self) -> CancellationToken {
        self.cancel.clone()
    }
    fn check(&self) -> Result<(), Error> {
        if self.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if Instant::now() >= self.limits.hard_end {
            return Err(Error::Timeout);
        }
        self.source.check_authority().map_err(network_error)
    }
    fn fail<T>(&mut self, error: Error) -> Result<T, Error> {
        self.failure = Some(error);
        self.segment.take();
        self.cursor.pending.clear();
        self.cancel.cancel();
        Err(error)
    }
    pub(super) async fn next_chunk(&mut self) -> Result<Option<Bytes>, Error> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        if let Err(error) = self.check() {
            return self.fail(error);
        }
        // Busy is backpressure, not a retry of a failed HTTP operation. No next
        // fetch/reload is started while an application still retains a chunk.
        let chunk = self
            .chunks
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let hold = self
            .released
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(hold) = hold {
            match hold.exclude_from(self.progress_deadline) {
                Ok(deadline) => self.progress_deadline = deadline.min(self.limits.hard_end),
                Err(error) => return self.fail(error),
            }
        }
        let result = self.next_bytes().await;
        match result {
            Ok(Some(bytes)) => Ok(Some(Bytes::from_owner(ChunkOwner {
                handed_off: Instant::now(),
                released: self.released.clone(),
                bytes,
                _source: self.source.clone(),
                _stream: self.stream.clone(),
                _chunk: chunk,
            }))),
            Ok(None) => Ok(None),
            Err(error) => self.fail(error),
        }
    }
    async fn next_bytes(&mut self) -> Result<Option<Bytes>, Error> {
        if self.segment.is_none() {
            while self.cursor.pending.is_empty() && !self.cursor.ended {
                self.reload().await?;
            }
            if self.cursor.pending.is_empty() {
                return Ok(None);
            }
            self.load_segment((Instant::now() + self.limits.idle).min(self.limits.hard_end))
                .await?;
        }
        if !self.segment.as_ref().ok_or(Error::InvalidResponse)?.started {
            // A valid EXTINF wait is bounded intentional pacing, not network
            // idle. Delayed consumers cannot cause a multi-segment catch-up burst.
            let due = self.next_due;
            checked(&self.source, &self.cancel, self.limits.hard_end, async {
                tokio::time::sleep_until(due).await;
                Ok(())
            })
            .await?;
            let segment = self.segment.as_mut().ok_or(Error::InvalidResponse)?;
            self.next_due = Instant::now().max(due) + segment.duration;
            segment.started = true;
        }
        self.check()?;
        let segment = self.segment.as_mut().ok_or(Error::InvalidResponse)?;
        let end = (segment.offset + CHUNK_BYTES).min(segment.fetched.bytes().len());
        let bytes = Bytes::copy_from_slice(&segment.fetched.bytes()[segment.offset..end]);
        segment.offset = end;
        if end == segment.fetched.bytes().len() {
            self.segment.take();
        }
        self.check()?;
        Ok(Some(bytes))
    }
    async fn load_segment(&mut self, deadline: Instant) -> Result<(), Error> {
        let segment = self.cursor.take()?.ok_or(Error::InvalidManifest)?;
        let fetched = checked(
            &self.source,
            &self.cancel,
            deadline,
            self.source.fetch_hls_resource(&segment.resource),
        )
        .await?;
        validate_ts(fetched.bytes())?;
        self.segment = Some(Segment {
            fetched,
            offset: 0,
            duration: segment.duration,
            started: false,
        });
        self.progress_deadline =
            Instant::now() + progress_wait(self.cursor.target, self.limits.idle);
        Ok(())
    }
    async fn reload(&mut self) -> Result<(), Error> {
        self.check()?;
        let deadline = (Instant::now() + self.limits.idle)
            .min(self.progress_deadline)
            .min(self.limits.hard_end);
        let fetched = if let Some(resource) = &self.playlist {
            checked(
                &self.source,
                &self.cancel,
                deadline,
                self.source.fetch_hls_resource(resource),
            )
            .await?
        } else {
            checked(
                &self.source,
                &self.cancel,
                deadline,
                self.source.fetch_root(),
            )
            .await?
        };
        let manifest = self.source.parse_hls(&fetched).map_err(network_error)?;
        let media = parse_media(&fetched, manifest)?;
        if self.cursor.reload(media)? {
            self.progress_deadline =
                Instant::now() + progress_wait(self.cursor.target, self.limits.idle);
        }
        if self.cursor.pending.is_empty() && !self.cursor.ended {
            let due = Instant::now() + (self.cursor.target / 2).max(Duration::from_millis(50));
            checked(
                &self.source,
                &self.cancel,
                self.progress_deadline.min(self.limits.hard_end),
                async {
                    tokio::time::sleep_until(due).await;
                    Ok(())
                },
            )
            .await?;
        }
        Ok(())
    }
}
impl Drop for HlsTsStream {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

fn parse_media(
    fetched: &Fetched,
    manifest: super::network::HlsManifest,
) -> Result<MediaPlaylist, Error> {
    if manifest
        .resources
        .iter()
        .any(|resource| resource.url.origin() != fetched.url.origin())
    {
        return Err(Error::DestinationDenied);
    }
    hls_timeline::parse(fetched, manifest).map_err(timeline_error)
}
fn progress_wait(target: Duration, idle: Duration) -> Duration {
    (target * 3).max(idle).min(MAX_PROGRESS_WAIT)
}
async fn checked<T>(
    source: &AuthorizedSource,
    cancel: &CancellationToken,
    deadline: Instant,
    future: impl Future<Output = Result<T, RuntimeError>>,
) -> Result<T, Error> {
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    source.check_authority().map_err(network_error)?;
    if Instant::now() >= deadline {
        return Err(Error::Timeout);
    }
    let result = tokio::select! { biased;
        _ = cancel.cancelled() => return Err(Error::Cancelled),
        result = tokio::time::timeout_at(deadline, future) => result.map_err(|_| Error::Timeout)?.map_err(network_error)?,
    };
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    source.check_authority().map_err(network_error)?;
    if Instant::now() >= deadline {
        return Err(Error::Timeout);
    }
    Ok(result)
}
fn network_error(error: RuntimeError) -> Error {
    match error {
        RuntimeError::Network(error) => error,
        _ => Error::Cancelled,
    }
}
fn timeline_error(error: TimelineError) -> Error {
    match error {
        TimelineError::TooLarge => Error::TooLarge,
        TimelineError::Provenance => Error::DestinationDenied,
        TimelineError::Unsupported => Error::Unsupported,
        _ => Error::InvalidManifest,
    }
}
fn validate_ts(bytes: &[u8]) -> Result<(), Error> {
    if bytes.len() > MAX_SEGMENT_BYTES {
        return Err(Error::TooLarge);
    }
    if bytes.len() < 5 * PACKET_BYTES || bytes.len() % PACKET_BYTES != 0 {
        return Err(Error::Unsupported);
    }
    check_packets(bytes, 0)?;
    for packet in bytes.chunks_exact(PACKET_BYTES) {
        if packet[1] & 0x80 != 0 {
            return Err(Error::Unsupported);
        }
        match packet[3] & 0x30 {
            0x20 if packet[4] != 183 => return Err(Error::Unsupported),
            0x30 if packet[4] > 182 => return Err(Error::Unsupported),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "stream_hls_tests.rs"]
pub(super) mod tests;
