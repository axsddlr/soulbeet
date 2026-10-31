//! Peer failover for downloads.
//!
//! A Soulseek peer can accept an enqueue and then never deliver a byte. The
//! search that picked it already returned other peers offering the same
//! track, so a failed transfer should move to the next-best source rather
//! than fail the track. When those run out, or the search offered none, the
//! track is searched for again. This module owns those alternates and the
//! per-track attempt budget; the waiters (`DownloadMonitor`, discovery's poll
//! loop) decide *when* a transfer has definitively failed.

use async_trait::async_trait;
use shared::download::{DownloadableGroup, DownloadableItem, EnqueueStatus, QueuedDownload};
use soulbeet::error::Result as SoulResult;
use soulbeet::DownloadBackend;
use std::collections::VecDeque;
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tokio::sync::{oneshot, Semaphore};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{info, warn};

use super::enqueue::{enqueue_claimed, transfer_key};
use crate::transfer_registry::{BatchRef, TransferRegistry};

/// Sources tried per track before it is declared failed: the peer picked at
/// search time plus five alternates, from the original search or a fresh
/// one. slskd retries each peer 3 times on its own (~40s) before reporting
/// failure, so this caps a dead track at roughly 4 minutes plus the
/// re-search.
pub const MAX_SOURCES_PER_TRACK: usize = 6;

/// Re-searches one slot may run. A stall can search while its peers are
/// offline, so a later failure must still get a fresh look.
pub const MAX_RESEARCHES_PER_SLOT: usize = 3;

/// The least time between two re-searches of the same slot, so peers have
/// a chance to come back before the next one.
pub const RESEARCH_COOLDOWN: Duration = Duration::from_secs(5 * 60);

/// Re-searches running at once across the whole process. slskd rate-limits
/// searches, and an album whose peer went away fails every track together.
pub const MAX_CONCURRENT_RESEARCHES: usize = 2;

/// The permits every pool's re-searches share, `MAX_CONCURRENT_RESEARCHES`
/// of them.
fn global_research_permits() -> Arc<Semaphore> {
    static PERMITS: LazyLock<Arc<Semaphore>> =
        LazyLock::new(|| Arc::new(Semaphore::new(MAX_CONCURRENT_RESEARCHES)));
    Arc::clone(&PERMITS)
}

/// How far a re-searched candidate's duration may stray from the original's,
/// at least, before it is taken for a different recording.
const DURATION_TOLERANCE_SECS: f64 = 10.0;

/// The same tolerance as a share of the original's duration, used when it is
/// the larger of the two.
const DURATION_TOLERANCE_SHARE: f64 = 0.05;

/// Searches for one track again once its alternates are spent. Returns the
/// search's result groups unfiltered: the pool keeps the ones that are the
/// same track. An empty `artist` searches the title alone. An `Err` is a
/// search that could not run or answer, as opposed to one that found
/// nothing.
///
/// `backend` is the one the pool enqueues into, so whatever it finds can be
/// downloaded from there.
#[async_trait]
pub trait AlternateFinder: Send + Sync {
    async fn search(
        &self,
        backend: &Arc<dyn DownloadBackend>,
        artist: &str,
        title: &str,
    ) -> SoulResult<Vec<DownloadableGroup>>;
}

/// The qualifiers that only describe the same recording, once any year in
/// them is set aside. Anything else ("Karaoke Version", "Demo", "Live",
/// "Kowloon Remix") names another recording and stays in the key.
const DROPPED_QUALIFIERS: &[&str] = &[
    "remaster",
    "remastered",
    "remaster version",
    "remastered version",
    "digital remaster",
    "digitally remastered",
    "radio edit",
    "single version",
    "album version",
    "original version",
    "explicit",
    "explicit version",
    "clean",
    "clean version",
    "mono",
    "stereo",
    "mono version",
    "stereo version",
    "deluxe",
    "deluxe edition",
    "deluxe version",
    "bonus track",
];

/// Whether `word` is a release year, as in "2011 Remaster" or "(1999)".
fn is_year(word: &str) -> bool {
    word.len() == 4
        && word.bytes().all(|b| b.is_ascii_digit())
        && (word.starts_with("19") || word.starts_with("20"))
}

/// Whether a bracketed or dashed qualifier, lowercased, only describes the
/// same recording: a featured artist, a bare year, or one of
/// `DROPPED_QUALIFIERS` with or without a year.
fn is_dropped_qualifier(qualifier: &str) -> bool {
    let words: Vec<&str> = qualifier
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    if matches!(words.first(), None | Some(&("feat" | "ft" | "featuring"))) {
        return true;
    }
    let words: Vec<&str> = words.into_iter().filter(|w| !is_year(w)).collect();
    words.is_empty() || DROPPED_QUALIFIERS.contains(&words.join(" ").as_str())
}

/// `title` without a leading track number. A number counts as one only when
/// a separator follows it ("03 - ", "03. ", "03_", "03) ", "3 - ") or when
/// it is zero-padded and followed by a space ("03 ", "007 "), so "99
/// Problems", "7 Rings" and "1.5 Miles" keep theirs. A title that is
/// nothing but a number keeps it too.
fn strip_track_number(title: &str) -> &str {
    let title = title.trim_start();
    let digits = title.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits > 3 {
        return title;
    }
    let after = &title[digits..];
    let spaced = after.trim_start();
    let rest = if let Some(rest) = spaced.strip_prefix(['-', '.', '_', ')']) {
        // "1.5 Miles" and "1-800" are numbers, not a track number and a title.
        if rest.starts_with(|c: char| c.is_ascii_digit()) {
            return title;
        }
        rest
    } else if digits >= 2 && title.starts_with('0') && after.starts_with(char::is_whitespace) {
        spaced
    } else {
        return title;
    };
    let rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '_');
    if !rest.chars().any(char::is_alphanumeric) {
        return title;
    }
    rest
}

/// `segment` up to a featured-artist marker ("ft.", "feat", "featuring"),
/// in any case.
fn strip_featured(segment: &str) -> String {
    segment
        .split_whitespace()
        .enumerate()
        .take_while(|(i, word)| {
            *i == 0
                || !matches!(
                    word.to_lowercase().as_str(),
                    "feat" | "feat." | "ft" | "ft." | "featuring"
                )
        })
        .map(|(_, word)| word)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The key two titles of the same recording share: lowercased, without a
/// leading track number, featured artists or the qualifiers
/// `is_dropped_qualifier` allows (remasters, radio edits, a year...), and
/// with punctuation and whitespace collapsed. Every other qualifier (live,
/// remix, acoustic, karaoke, demo...) stays, since those are different
/// recordings. Empty when nothing is left, and an empty key matches nothing.
pub fn title_key(title: &str) -> String {
    let lower = title.to_lowercase().replace(['\u{2013}', '\u{2014}'], "-");
    let mut rest = strip_track_number(&lower);

    // Bracketed qualifiers first, so a dash inside one cannot split it.
    let mut kept = String::new();
    while let Some(open) = rest.find(['(', '[']) {
        kept.push_str(&rest[..open]);
        let close = if rest[open..].starts_with('(') {
            ')'
        } else {
            ']'
        };
        let inner = &rest[open + 1..];
        let Some(len) = inner.find(close) else {
            rest = &rest[open..];
            break;
        };
        if !is_dropped_qualifier(&inner[..len]) {
            kept.push(' ');
            kept.push_str(&inner[..len]);
            kept.push(' ');
        }
        rest = &inner[len + 1..];
    }
    kept.push_str(rest);

    let segments: Vec<String> = kept
        .split(" - ")
        .enumerate()
        .filter(|(i, segment)| *i == 0 || !is_dropped_qualifier(segment))
        .map(|(_, segment)| strip_featured(segment))
        .collect();
    shared::recommendation::normalize_for_matching(&segments.join(" "))
}

/// The title a re-search sends: `title` without what `title_key` drops (a
/// leading track number, featured artists, the qualifiers
/// `is_dropped_qualifier` allows) and with its whitespace collapsed, but in
/// its own case and words otherwise. Every other qualifier stays, as it
/// names the recording wanted. Never empty: a title that cleans down to
/// nothing goes out as it is, trimmed.
pub fn query_title(title: &str) -> String {
    let raw = title.trim();
    let dashed = raw.replace(['\u{2013}', '\u{2014}'], "-");
    let mut rest = strip_track_number(&dashed);

    // Bracketed qualifiers first, so a dash inside one cannot split it.
    let mut kept = String::new();
    while let Some(open) = rest.find(['(', '[']) {
        kept.push_str(&rest[..open]);
        let close = if rest[open..].starts_with('(') {
            ')'
        } else {
            ']'
        };
        let inner = &rest[open + 1..];
        let Some(len) = inner.find(close) else {
            rest = &rest[open..];
            break;
        };
        if !is_dropped_qualifier(&inner[..len].to_lowercase()) {
            kept.push(' ');
            kept.push_str(&rest[open..open + len + 2]);
            kept.push(' ');
        }
        rest = &inner[len + 1..];
    }
    kept.push_str(rest);

    let segments: Vec<String> = kept
        .split(" - ")
        .enumerate()
        .filter(|(i, segment)| *i == 0 || !is_dropped_qualifier(&segment.to_lowercase()))
        .map(|(_, segment)| strip_featured(segment))
        .filter(|segment| !segment.is_empty())
        .collect();
    let cleaned = segments.join(" - ");
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.is_empty() {
        raw.to_string()
    } else {
        cleaned
    }
}

/// Whether a candidate's duration is close enough to the original's to be
/// the same recording. Unknown on either side cannot be judged, so it passes.
fn duration_matches(original: Option<u32>, candidate: Option<u32>) -> bool {
    let (Some(original), Some(candidate)) = (original, candidate) else {
        return true;
    };
    let tolerance = DURATION_TOLERANCE_SECS.max(f64::from(original) * DURATION_TOLERANCE_SHARE);
    (f64::from(original) - f64::from(candidate)).abs() <= tolerance
}

/// `artist` lowercased for comparison: "&" read as "and", and path
/// separators, underscores and other punctuation turned into spaces before
/// `normalize_for_matching`, so a folder name splits into its words.
fn normalize_artist(artist: &str) -> String {
    let spaced: String = artist
        .replace('&', " and ")
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '\'' {
                c
            } else {
                ' '
            }
        })
        .collect();
    shared::recommendation::normalize_for_matching(&spaced)
}

/// The words of a normalized artist that tell it apart, in order: without
/// "the" and "and", which differ between spellings of the same name.
fn significant_words(artist: &str) -> Vec<&str> {
    let mut words: Vec<&str> = artist
        .split(' ')
        .filter(|w| !matches!(*w, "" | "the" | "and"))
        .collect();
    words.sort_unstable();
    words.dedup();
    words
}

/// Whether the normalized `needle` appears in the normalized `haystack` as
/// whole words. A needle of only "the"/"and" never does.
fn contains_words(haystack: &str, needle: &str) -> bool {
    !significant_words(needle).is_empty()
        && format!(" {haystack} ").contains(&format!(" {needle} "))
}

/// Whether two normalized artists name the same act: one holds the other
/// ("Kowloon" in "Kowloon and Friends"), or they share every significant
/// word ("The Beatles" and "Beatles, The").
fn artists_match(a: &str, b: &str) -> bool {
    if contains_words(a, b) || contains_words(b, a) {
        return true;
    }
    let words = significant_words(a);
    !words.is_empty() && words == significant_words(b)
}

/// Whether `candidate` is by `artist`, normalized and non-empty: its artist
/// field names it, or its path does. slskd fills the artist field with a
/// guess from the path, made against the searched artist, so a title-only
/// search's guess can be any folder; the full path still names the artist
/// when a folder does.
fn artist_confirmed(artist: &str, candidate: &DownloadableItem) -> bool {
    let named = normalize_artist(&candidate.artist);
    if artists_match(artist, &named) {
        return true;
    }
    let path = normalize_artist(&candidate.id);
    let bare = artist.strip_prefix("the ").unwrap_or(artist);
    contains_words(&path, artist) || contains_words(&path, bare)
}

/// The items of a re-search that can stand in for `track`, best group
/// first: the same non-empty title key, the same artist when the track has
/// one (`artist_confirmed`), a close duration, and a source not `tried`.
/// When the track's duration is known, candidates of unknown duration come
/// after the ones known to match.
fn usable_candidates(
    track: &DownloadableItem,
    mut groups: Vec<DownloadableGroup>,
    tried: &[(String, String)],
) -> Vec<DownloadableItem> {
    let wanted = title_key(&track.title);
    if wanted.is_empty() {
        return Vec::new();
    }
    groups.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let artist = normalize_artist(&track.artist);
    let mut found: Vec<DownloadableItem> = Vec::new();
    for candidate in groups.into_iter().flat_map(|group| group.items) {
        let seen = |source: &str, id: &str| source == candidate.source && id == candidate.id;
        if title_key(&candidate.title) == wanted
            && (artist.is_empty() || artist_confirmed(&artist, &candidate))
            && duration_matches(track.duration, candidate.duration)
            && !tried.iter().any(|(source, id)| seen(source, id))
            && !found.iter().any(|f| seen(&f.source, &f.id))
        {
            found.push(candidate);
        }
    }
    if track.duration.is_some() {
        // Stable, so each half keeps its score order.
        found.sort_by_key(|candidate| candidate.duration.is_none());
    }
    found
}

/// Search for `track` again: by artist and cleaned title (`query_title`),
/// then by the title alone only when that finds nothing usable. Both
/// queries keep only candidates by the track's artist. An error from
/// either query ends the re-search with that error.
async fn research(
    finder: &dyn AlternateFinder,
    backend: &Arc<dyn DownloadBackend>,
    track: &DownloadableItem,
    tried: &[(String, String)],
) -> SoulResult<Vec<DownloadableItem>> {
    let title = query_title(&track.title);
    let title = title.as_str();
    let artist = track.artist.trim();
    let mut queries = Vec::new();
    if !artist.is_empty() {
        queries.push(artist);
    }
    queries.push("");
    for artist in queries {
        let groups = match finder.search(backend, artist, title).await {
            Ok(groups) => groups,
            Err(e) => {
                warn!("Re-search for '{} {}' failed: {}", artist, title, e);
                return Err(e);
            }
        };
        let found = usable_candidates(track, groups, tried);
        info!(
            "Re-search for '{} {}' found {} new source(s)",
            artist,
            title,
            found.len()
        );
        if !found.is_empty() {
            return Ok(found);
        }
    }
    Ok(Vec::new())
}

/// What one slot needs to search again, detached from the pool so the
/// search can run on a task of its own.
struct ResearchJob {
    finder: Arc<dyn AlternateFinder>,
    track: DownloadableItem,
    tried: Vec<(String, String)>,
    /// Held for both of the re-search's queries.
    permits: Arc<Semaphore>,
}

impl ResearchJob {
    /// Run the re-search once a permit is free. `None` when it errored.
    async fn run(&self, backend: &Arc<dyn DownloadBackend>) -> Option<Vec<DownloadableItem>> {
        // The semaphore is never closed, so this always holds a permit.
        let _permit = self.permits.acquire().await;
        research(self.finder.as_ref(), backend, &self.track, &self.tried)
            .await
            .ok()
    }
}

/// A re-search running on its own task, or waiting there for a permit.
/// Dropping it, with the pool or the monitor, stops the search: nobody is
/// left to use what it finds. `found` carries `None` when it errored.
struct PendingResearch {
    found: oneshot::Receiver<Option<Vec<DownloadableItem>>>,
    task: JoinHandle<()>,
}

impl Drop for PendingResearch {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// What made a slot look for another source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// The transfer stopped moving but has not failed.
    Stall,
    /// The transfer failed, timed out, or vanished from the backend.
    Failure,
}

/// What `SourcePool::enqueue_or_search` did for a slot.
pub enum Failover {
    /// An alternate was accepted into the batch folder.
    Enqueued(QueuedDownload),
    /// A re-search is running for the slot, so a later call may have a
    /// source. The slot keeps its transfer meanwhile.
    Searching,
    /// The slot is out of sources for now: its budget is spent, or no
    /// re-search may run yet.
    Exhausted,
}

/// Alternates and attempt budget for one track.
struct Slot {
    /// The track this slot downloads, which a re-search looks for. `None`
    /// when it could not be recovered, so the slot never re-searches.
    track: Option<DownloadableItem>,
    alternates: VecDeque<DownloadableItem>,
    /// `(source, id)` of every source this slot was given, the original
    /// included, so a re-search never offers one of them again.
    tried: Vec<(String, String)>,
    /// How many re-searches this slot started, at most
    /// `MAX_RESEARCHES_PER_SLOT`. One that errored is given back.
    researches: usize,
    /// When the last re-search started; the next waits `RESEARCH_COOLDOWN`,
    /// errored or not.
    researched_at: Option<Instant>,
    /// What started the last re-search that answered or is running. A
    /// failure may search inside the cooldown after a stall's search.
    researched_for: Option<Trigger>,
    /// The re-search running for this slot, at most one.
    pending: Option<PendingResearch>,
    used: usize,
}

impl Slot {
    fn new(track: Option<DownloadableItem>, alternates: VecDeque<DownloadableItem>) -> Self {
        let tried = track
            .iter()
            .map(|t| (t.source.clone(), t.id.clone()))
            .collect();
        Self {
            track,
            alternates,
            tried,
            researches: 0,
            researched_at: None,
            researched_for: None,
            pending: None,
            used: 1,
        }
    }

    /// Take in a finished re-search: its finds, or, when it errored
    /// (`None`), its research back. An errored search still holds off the
    /// next one for the cooldown, a failure's included: slskd may be
    /// refusing searches.
    fn settle_research(&mut self, outcome: Option<Vec<DownloadableItem>>) {
        match outcome {
            Some(found) => self.absorb(found),
            None => {
                self.researches = self.researches.saturating_sub(1);
                self.researched_for = None;
            }
        }
    }

    /// Queue a re-search's finds behind the alternates, minus any source
    /// this slot was given or holds since the search started.
    fn absorb(&mut self, found: Vec<DownloadableItem>) {
        for item in found {
            let same = |source: &str, id: &str| source == item.source && id == item.id;
            if !self.tried.iter().any(|(source, id)| same(source, id))
                && !self.alternates.iter().any(|a| same(&a.source, &a.id))
            {
                self.alternates.push_back(item);
            }
        }
    }
}

/// Ranked alternate sources for every track in a batch, indexed by slot.
pub struct SourcePool {
    slots: Vec<Slot>,
    /// Searches again for a slot whose alternates ran out; `None` disables
    /// re-searching.
    finder: Option<Arc<dyn AlternateFinder>>,
    /// Bounds the re-searches running at once: the process-wide permits,
    /// unless a test gives the pool its own.
    permits: Arc<Semaphore>,
}

impl SourcePool {
    /// Alternates for a batch, indexed by the slot the caller will monitor.
    ///
    /// Callers pass the tracks in the exact order they were enqueued, which is
    /// not the order they were searched: slskd groups an enqueue by peer, and
    /// items whose backend data will not parse are dropped before enqueue. A
    /// `None` entry keeps a slot aligned when its track cannot be recovered,
    /// and that slot simply gets no alternates.
    ///
    /// `rest` must already be in descending score order, which is how
    /// `auto_download` sorts it. Matching is by `title_key`, never by
    /// filename: the same track has a completely different remote path on
    /// each peer.
    pub fn from_tracks(tracks: &[Option<DownloadableItem>], rest: &[DownloadableGroup]) -> Self {
        let slots = tracks
            .iter()
            .map(|track| {
                let alternates = match track {
                    Some(track) if !title_key(&track.title).is_empty() => {
                        let wanted = title_key(&track.title);
                        rest.iter()
                            .flat_map(|group| group.items.iter())
                            .filter(|candidate| title_key(&candidate.title) == wanted)
                            .cloned()
                            .collect()
                    }
                    _ => VecDeque::new(),
                };
                Slot::new(track.clone(), alternates)
            })
            .collect();

        Self {
            slots,
            finder: None,
            permits: global_research_permits(),
        }
    }

    /// Single track, N ranked sources. `items[0]` is the source already
    /// enqueued; the rest are its alternates.
    pub fn single(items: Vec<DownloadableItem>) -> Self {
        // Workaround for a std bug (seen on 1.90-1.92): collecting a fully
        // drained `vec::IntoIter` into a `VecDeque` reuses the buffer with
        // `head == capacity`, so a later `extend` + `pop_front` reads past the
        // allocation. `VecDeque::from(Vec)` + `pop_front` keeps `head` in
        // bounds. `research_drops_candidates_whose_duration_is_off` crashed
        // on this.
        let mut items = VecDeque::from(items);
        let track = items.pop_front();
        Self {
            slots: vec![Slot::new(track, items)],
            finder: None,
            permits: global_research_permits(),
        }
    }

    /// Search again for a slot whose alternates ran out, or that had none.
    pub fn with_finder(mut self, finder: Arc<dyn AlternateFinder>) -> Self {
        self.finder = Some(finder);
        self
    }

    /// Test hook: cap this pool's concurrent re-searches at `limit`, apart
    /// from every other pool, instead of sharing the process-wide cap.
    #[cfg(test)]
    pub(crate) fn with_research_limit(mut self, limit: usize) -> Self {
        self.permits = Arc::new(Semaphore::new(limit));
        self
    }

    /// How many sources this slot has consumed, including the original.
    pub fn attempts_used(&self, slot: usize) -> usize {
        self.slots.get(slot).map(|s| s.used).unwrap_or(0)
    }

    /// Next alternate for this slot, or `None` once the budget is spent, the
    /// alternates run out, or the slot does not exist.
    pub fn take_next(&mut self, slot: usize) -> Option<DownloadableItem> {
        let slot = self.slots.get_mut(slot)?;
        if slot.used >= MAX_SOURCES_PER_TRACK {
            return None;
        }
        let item = slot.alternates.pop_front()?;
        slot.used += 1;
        slot.tried.push((item.source.clone(), item.id.clone()));
        Some(item)
    }

    /// Start this slot's next re-search, if it may have one now: a finder
    /// is set, the track is known, the source budget is not spent, no
    /// re-search is running, fewer than `MAX_RESEARCHES_PER_SLOT` ran, and
    /// `RESEARCH_COOLDOWN` passed since the last one began. A `Failure`
    /// skips the cooldown once after a `Stall`'s search: the stall searched
    /// while the peers may have been away, and a failure is final.
    fn research_job(&mut self, slot: usize, trigger: Trigger) -> Option<ResearchJob> {
        let finder = self.finder.clone()?;
        let permits = Arc::clone(&self.permits);
        let entry = self.slots.get_mut(slot)?;
        let after_stall =
            trigger == Trigger::Failure && entry.researched_for == Some(Trigger::Stall);
        let cooled = after_stall
            || entry
                .researched_at
                .is_none_or(|at| at.elapsed() >= RESEARCH_COOLDOWN);
        if entry.pending.is_some()
            || entry.used >= MAX_SOURCES_PER_TRACK
            || entry.researches >= MAX_RESEARCHES_PER_SLOT
            || !cooled
        {
            return None;
        }
        let track = entry.track.clone()?;
        entry.researches += 1;
        entry.researched_at = Some(Instant::now());
        entry.researched_for = Some(trigger);
        Some(ResearchJob {
            finder,
            track,
            tried: entry.tried.clone(),
            permits,
        })
    }

    /// Take in this slot's running re-search if it has finished. Returns
    /// whether one is still running.
    fn collect_research(&mut self, slot: usize) -> bool {
        let Some(entry) = self.slots.get_mut(slot) else {
            return false;
        };
        let Some(pending) = entry.pending.as_mut() else {
            return false;
        };
        let outcome = match pending.found.try_recv() {
            Ok(outcome) => outcome,
            Err(oneshot::error::TryRecvError::Empty) => return true,
            // The task ended without an answer: it found nothing.
            Err(oneshot::error::TryRecvError::Closed) => Some(Vec::new()),
        };
        entry.pending = None;
        entry.settle_research(outcome);
        false
    }

    /// Start a re-search for this slot on a task of its own, which waits
    /// there for a permit first. Returns false when the slot may not
    /// re-search now.
    fn spawn_research(
        &mut self,
        slot: usize,
        backend: &Arc<dyn DownloadBackend>,
        trigger: Trigger,
    ) -> bool {
        let Some(job) = self.research_job(slot, trigger) else {
            return false;
        };
        let (tx, found) = oneshot::channel();
        let backend = Arc::clone(backend);
        let task = tokio::spawn(async move {
            let _ = tx.send(job.run(&backend).await);
        });
        self.slots[slot].pending = Some(PendingResearch { found, task });
        true
    }

    /// Test hook: pretend this slot's last re-search started `by` ago.
    #[cfg(test)]
    pub(crate) fn backdate_research(&mut self, slot: usize, by: Duration) {
        self.slots[slot].researched_at = Some(
            Instant::now()
                .checked_sub(by)
                .expect("monotonic clock younger than the window under test"),
        );
    }

    /// Test hook: wait for this slot's running re-search, if any, and take
    /// in what it found.
    #[cfg(test)]
    pub(crate) async fn research_settled(&mut self, slot: usize) {
        let entry = &mut self.slots[slot];
        let Some(mut pending) = entry.pending.take() else {
            return;
        };
        let outcome = tokio::time::timeout(Duration::from_secs(5), &mut pending.found)
            .await
            .expect("the re-search never finished")
            .unwrap_or(Some(Vec::new()));
        entry.settle_research(outcome);
    }

    /// `take_next`, re-searching on `backend` and waiting for it, permit
    /// included, when the slot's alternates are spent but its budget is not.
    /// Its callers fail over only on a failure.
    async fn next_source(
        &mut self,
        slot: usize,
        backend: &Arc<dyn DownloadBackend>,
    ) -> Option<DownloadableItem> {
        if let Some(item) = self.take_next(slot) {
            return Some(item);
        }
        let job = self.research_job(slot, Trigger::Failure)?;
        let outcome = job.run(backend).await;
        self.slots[slot].settle_research(outcome);
        self.take_next(slot)
    }

    /// Enqueue this slot's next alternate into `batch`'s folder, walking past
    /// the ones it cannot have (another batch owns the transfer, the peer is
    /// offline, slskd is already fetching it) until one is accepted or the
    /// slot runs dry. The accepted alternate's key stays claimed by `batch`.
    /// Once the static alternates are spent the slot re-searches, waiting
    /// for it, within its re-search budget and cooldown. A refused alternate
    /// still costs the slot a source, so five dead peers in a row spend the
    /// whole budget without a transfer ever being attempted. `None` means
    /// the slot is out of sources or budget.
    pub async fn enqueue_next(
        &mut self,
        slot: usize,
        backend: &Arc<dyn DownloadBackend>,
        batch: &BatchRef,
    ) -> Option<QueuedDownload> {
        while let Some(item) = self.next_source(slot, backend).await {
            if let Some(queued) = enqueue_alternate(item, backend, batch).await {
                return Some(queued);
            }
        }
        None
    }

    /// `enqueue_next` for a poll loop, which must not wait on a search: when
    /// the slot's alternates are spent, the re-search is started on a task
    /// of its own and `Searching` returned. A later call takes in what it
    /// found and enqueues from that. While a re-search is running, or
    /// waiting for a permit, the slot starts no other. `trigger` is what
    /// made the caller fail over, which decides whether a failure may
    /// search inside the cooldown (`research_job`).
    pub async fn enqueue_or_search(
        &mut self,
        slot: usize,
        backend: &Arc<dyn DownloadBackend>,
        batch: &BatchRef,
        trigger: Trigger,
    ) -> Failover {
        if self.collect_research(slot) {
            return Failover::Searching;
        }
        loop {
            let Some(item) = self.take_next(slot) else {
                return if self.spawn_research(slot, backend, trigger) {
                    Failover::Searching
                } else {
                    Failover::Exhausted
                };
            };
            if let Some(queued) = enqueue_alternate(item, backend, batch).await {
                return Failover::Enqueued(queued);
            }
        }
    }
}

/// Claim and enqueue one alternate into `batch`'s folder. `None` when
/// another batch owns the transfer or the backend did not accept it.
async fn enqueue_alternate(
    item: DownloadableItem,
    backend: &Arc<dyn DownloadBackend>,
    batch: &BatchRef,
) -> Option<QueuedDownload> {
    let source = item.source.clone();
    let key = transfer_key(backend.id(), &item.source, &item.id);
    if TransferRegistry::global()
        .claim(&[key], batch)
        .claimed
        .is_empty()
    {
        warn!(
            "Failover source {} skipped: another batch is downloading {}",
            source, item.id
        );
        return None;
    }
    match enqueue_claimed(backend, vec![item], batch).await {
        Ok(queued) => {
            if let Some(ok) = queued
                .into_iter()
                .find(|d| d.status == EnqueueStatus::Enqueued)
            {
                info!("Failover enqueued {} from {}", ok.item, ok.source);
                return Some(ok);
            }
            warn!("Failover source {} refused the enqueue", source);
        }
        Err(e) => warn!("Failover enqueue to {} failed: {}", source, e),
    }
    None
}

/// Line the accepted downloads up with the tracks they came from, in the
/// order the caller will monitor them.
///
/// `queued` is neither the order nor the length of `items`: slskd groups an
/// enqueue by peer, items whose backend data will not parse are dropped
/// before the request is sent, and rejected enqueues are filtered out by the
/// caller.
/// Feeding `items` straight to `from_tracks` would hand slot N the alternates
/// of a different track.
///
/// Matching keys on both `id` and `source`: `QueuedDownload::item` is the
/// item's `id` verbatim, both being the remote filename, and the same
/// filename can be offered by more than one peer. A download that matches no
/// item stays `None` so the slots after it keep their index.
pub fn align_pool_tracks(
    queued: &[QueuedDownload],
    items: &[DownloadableItem],
) -> Vec<Option<DownloadableItem>> {
    queued
        .iter()
        .map(|d| {
            items
                .iter()
                .find(|item| item.id == d.item && item.source == d.source)
                .cloned()
        })
        .collect()
}

#[cfg(all(test, feature = "server"))]
mod tests {
    use super::*;
    use crate::server_fns::download::test_support::{
        batch, group, item, key, ScriptedFinder, ScriptedOutcome, StubBackend,
    };
    use crate::transfer_registry::TransferRegistry;
    use shared::download::DownloadableItem;
    use soulbeet::DownloadBackend;
    use std::sync::Arc;
    use std::time::Duration;

    fn queued(source: &str, item: &str) -> QueuedDownload {
        QueuedDownload {
            id: item.to_string(),
            source: source.to_string(),
            item: item.to_string(),
            size: 0,
            status: EnqueueStatus::Enqueued,
        }
    }

    /// The ids `align_pool_tracks` lands in each slot, `None` for a gap.
    fn aligned_ids(queued: &[QueuedDownload], items: &[DownloadableItem]) -> Vec<Option<String>> {
        align_pool_tracks(queued, items)
            .into_iter()
            .map(|track| track.map(|i| i.id))
            .collect()
    }

    /// Three tracks of one album as the search returned them, `dirA` first.
    fn searched() -> Vec<DownloadableItem> {
        vec![
            item("peer_a", "One", "dirA/01.flac"),
            item("peer_a", "Two", "dirB/02.flac"),
            item("peer_a", "Three", "dirB/03.flac"),
        ]
    }

    #[test]
    fn matches_alternates_by_title_across_differing_paths() {
        let tracks = vec![Some(item(
            "ElevatorsForPeople",
            "Wake Up",
            "music\\Kowloon\\Come Over (2021)\\Kowloon - Come Over - 07 - Wake Up - [FLAC 44.1kHz].flac",
        ))];
        let rest = vec![group(
            "Cornflake9026",
            vec![item(
                "Cornflake9026",
                "Wake Up ",
                "music\\Kowloon\\Kowloon - Come Over\\07 - Wake Up.flac",
            )],
            0.9,
        )];

        let mut pool = SourcePool::from_tracks(&tracks, &rest);
        let next = pool.take_next(0).expect("alternate for slot 0");
        assert_eq!(next.source, "Cornflake9026");
    }

    #[test]
    fn alternates_follow_the_order_of_the_remaining_groups() {
        let tracks = vec![Some(item("peer_a", "Wake Up", "a.flac"))];
        let rest = vec![
            group("peer_b", vec![item("peer_b", "Wake Up", "b.flac")], 0.9),
            group("peer_c", vec![item("peer_c", "Wake Up", "c.flac")], 0.8),
            group("peer_d", vec![item("peer_d", "Wake Up", "d.flac")], 0.7),
        ];

        let mut pool = SourcePool::from_tracks(&tracks, &rest);
        assert_eq!(pool.take_next(0).map(|i| i.source), Some("peer_b".into()));
        assert_eq!(pool.take_next(0).map(|i| i.source), Some("peer_c".into()));
        assert_eq!(pool.take_next(0).map(|i| i.source), Some("peer_d".into()));
        assert!(pool.take_next(0).is_none());
    }

    #[test]
    fn align_pool_tracks_follows_the_enqueue_order() {
        // The enqueue came back with the dirB files first. Returning `items` in search order here would give
        // slot 0 the alternates for "One" while the monitor watches "Two".
        let items = searched();
        let accepted = vec![
            queued("peer_a", "dirB/02.flac"),
            queued("peer_a", "dirB/03.flac"),
            queued("peer_a", "dirA/01.flac"),
        ];

        assert_eq!(
            aligned_ids(&accepted, &items),
            vec![
                Some("dirB/02.flac".to_string()),
                Some("dirB/03.flac".to_string()),
                Some("dirA/01.flac".to_string()),
            ]
        );
    }

    #[test]
    fn align_pool_tracks_skips_an_item_dropped_before_enqueue() {
        // "Two" had unparseable backend data, so to_slskd_track dropped it and
        // slskd was never asked about it. Three items, two slots.
        let items = searched();
        let accepted = vec![
            queued("peer_a", "dirA/01.flac"),
            queued("peer_a", "dirB/03.flac"),
        ];

        assert_eq!(
            aligned_ids(&accepted, &items),
            vec![
                Some("dirA/01.flac".to_string()),
                Some("dirB/03.flac".to_string()),
            ]
        );
    }

    #[test]
    fn align_pool_tracks_skips_a_rejected_enqueue() {
        // "Two" came back with an error and the caller partitioned it out, and
        // the survivors arrived in another order than the search's.
        let items = searched();
        let accepted = vec![
            queued("peer_a", "dirB/03.flac"),
            queued("peer_a", "dirA/01.flac"),
        ];

        assert_eq!(
            aligned_ids(&accepted, &items),
            vec![
                Some("dirB/03.flac".to_string()),
                Some("dirA/01.flac".to_string()),
            ]
        );
    }

    #[test]
    fn align_pool_tracks_leaves_an_unrecognised_download_as_a_gap() {
        // Same remote filename, different peer: the source half of the key
        // must stop it matching, and the slot must stay in place so the
        // download after it is not shifted down.
        let items = searched();
        let accepted = vec![
            queued("peer_a", "dirA/01.flac"),
            queued("peer_z", "dirB/02.flac"),
            queued("peer_a", "dirB/03.flac"),
        ];

        assert_eq!(
            aligned_ids(&accepted, &items),
            vec![
                Some("dirA/01.flac".to_string()),
                None,
                Some("dirB/03.flac".to_string()),
            ]
        );
    }

    #[test]
    fn from_tracks_slots_follow_the_given_order() {
        // The enqueue order is not the search order: slskd returns its enqueue
        // results grouped by peer, so slot 0 here is
        // the track the alternate group happens to list second. Slots must
        // follow the tracks handed in, never the order found in `rest`.
        let rest = vec![group(
            "peer_b",
            vec![
                item("peer_b", "Come Over", "b-come-over.flac"),
                item("peer_b", "Wake Up", "b-wake-up.flac"),
            ],
            0.9,
        )];
        let tracks = vec![
            Some(item("peer_a", "Wake Up", "a-wake-up.flac")),
            Some(item("peer_a", "Come Over", "a-come-over.flac")),
        ];

        let mut pool = SourcePool::from_tracks(&tracks, &rest);
        assert_eq!(
            pool.take_next(0).map(|i| i.id),
            Some("b-wake-up.flac".to_string())
        );
        assert_eq!(
            pool.take_next(1).map(|i| i.id),
            Some("b-come-over.flac".to_string())
        );
    }

    #[test]
    fn an_unrecoverable_track_keeps_the_slots_after_it_aligned() {
        let rest = vec![group(
            "peer_b",
            vec![
                item("peer_b", "Wake Up", "b-wake-up.flac"),
                item("peer_b", "Come Over", "b-come-over.flac"),
            ],
            0.9,
        )];
        // Slot 1's track could not be matched back to the picked group. It
        // must still take a slot: compacting it away would shift "Come Over"
        // down to slot 1 and hand slot 2's failover the wrong track.
        let tracks = vec![
            Some(item("peer_a", "Wake Up", "a-wake-up.flac")),
            None,
            Some(item("peer_a", "Come Over", "a-come-over.flac")),
        ];

        let mut pool = SourcePool::from_tracks(&tracks, &rest);
        assert_eq!(
            pool.take_next(0).map(|i| i.id),
            Some("b-wake-up.flac".to_string())
        );
        assert!(pool.take_next(1).is_none());
        assert_eq!(
            pool.take_next(2).map(|i| i.id),
            Some("b-come-over.flac".to_string())
        );
    }

    #[test]
    fn ignores_alternates_for_a_different_title() {
        let tracks = vec![Some(item("peer_a", "Wake Up", "a.flac"))];
        let rest = vec![group(
            "peer_b",
            vec![item("peer_b", "Come Over", "b.flac")],
            0.9,
        )];

        let mut pool = SourcePool::from_tracks(&tracks, &rest);
        assert!(pool.take_next(0).is_none());
    }

    #[test]
    fn stops_after_the_source_budget_is_spent() {
        let items: Vec<DownloadableItem> = (0..8)
            .map(|i| item(&format!("peer_{i}"), "Wake Up", &format!("{i}.flac")))
            .collect();

        let mut pool = SourcePool::single(items);
        // slot starts having used its first source, so 5 alternates remain
        assert_eq!(pool.take_next(0).map(|i| i.source), Some("peer_1".into()));
        assert_eq!(pool.take_next(0).map(|i| i.source), Some("peer_2".into()));
        assert_eq!(pool.take_next(0).map(|i| i.source), Some("peer_3".into()));
        assert_eq!(pool.take_next(0).map(|i| i.source), Some("peer_4".into()));
        assert_eq!(pool.take_next(0).map(|i| i.source), Some("peer_5".into()));
        assert!(pool.take_next(0).is_none());
        assert_eq!(pool.attempts_used(0), MAX_SOURCES_PER_TRACK);
    }

    #[test]
    fn dry_pool_and_unknown_slot_return_none() {
        let mut pool = SourcePool::single(vec![item("peer_a", "Wake Up", "a.flac")]);
        assert!(pool.take_next(0).is_none());
        assert!(pool.take_next(9).is_none());
    }

    #[tokio::test]
    async fn enqueues_the_next_alternate() {
        let items = vec![
            item("peer_a", "Wake Up", "a.flac"),
            item("peer_b", "Wake Up", "b.flac"),
        ];
        let mut pool = SourcePool::single(items);
        let backend: Arc<dyn DownloadBackend> = StubBackend::new(vec![ScriptedOutcome::Enqueued]);

        let queued = pool
            .enqueue_next(0, &backend, &batch("X"))
            .await
            .expect("enqueued");
        assert_eq!(queued.source, "peer_b");
    }

    #[tokio::test]
    async fn skips_an_alternate_whose_enqueue_errors() {
        let items = vec![
            item("peer_a", "Wake Up", "a.flac"),
            item("peer_b", "Wake Up", "b.flac"),
            item("peer_c", "Wake Up", "c.flac"),
        ];
        let mut pool = SourcePool::single(items);
        // peer_b's enqueue is rejected (Ok response, item status Failed), peer_c's succeeds
        let backend: Arc<dyn DownloadBackend> =
            StubBackend::new(vec![ScriptedOutcome::Rejected, ScriptedOutcome::Enqueued]);

        let queued = pool
            .enqueue_next(0, &backend, &batch("X"))
            .await
            .expect("enqueued");
        assert_eq!(queued.source, "peer_c");
    }

    #[tokio::test]
    async fn skips_an_alternate_whose_transport_call_errors() {
        let items = vec![
            item("peer_a", "Wake Up", "a.flac"),
            item("peer_b", "Wake Up", "b.flac"),
            item("peer_c", "Wake Up", "c.flac"),
        ];
        let mut pool = SourcePool::single(items);
        // peer_b's download() call itself fails, peer_c's succeeds
        let backend: Arc<dyn DownloadBackend> = StubBackend::new(vec![
            ScriptedOutcome::TransportError,
            ScriptedOutcome::Enqueued,
        ]);

        let queued = pool
            .enqueue_next(0, &backend, &batch("X"))
            .await
            .expect("enqueued");
        assert_eq!(queued.source, "peer_c");
    }

    #[tokio::test]
    async fn already_in_flight_alternate_is_skipped() {
        let items = vec![
            item("peer_a", "Wake Up", "a.flac"),
            item("peer_b", "Wake Up", "b.flac"),
            item("peer_c", "Wake Up", "c.flac"),
        ];
        let mut pool = SourcePool::single(items);
        let backend: Arc<dyn DownloadBackend> = StubBackend::new(vec![
            ScriptedOutcome::AlreadyInFlight,
            ScriptedOutcome::Enqueued,
        ]);

        let queued = pool
            .enqueue_next(0, &backend, &batch("X"))
            .await
            .expect("enqueued");
        assert_eq!(queued.source, "peer_c");
        assert_eq!(queued.id, "t-c.flac");
        assert_eq!(pool.attempts_used(0), 3);
    }

    #[tokio::test]
    async fn failover_enqueues_into_the_batch_folder() {
        let items = vec![
            item("peer_a", "Wake Up", "a.flac"),
            item("peer_b", "Wake Up", "b.flac"),
        ];
        let mut pool = SourcePool::single(items);
        let stub = StubBackend::new(vec![ScriptedOutcome::Enqueued]);
        let backend: Arc<dyn DownloadBackend> = stub.clone();

        pool.enqueue_next(0, &backend, &batch("X"))
            .await
            .expect("enqueued");
        assert_eq!(stub.destinations(), vec!["X [12345678]".to_string()]);
    }

    #[tokio::test]
    async fn a_claimed_key_is_released_when_slskd_says_already_in_flight() {
        let items = vec![
            item("peer_a", "Wake Up", "a.flac"),
            item("peer_b", "Wake Up", "b.flac"),
            item("peer_c", "Wake Up", "c.flac"),
        ];
        let mut pool = SourcePool::single(items);
        let backend: Arc<dyn DownloadBackend> = StubBackend::new(vec![
            ScriptedOutcome::AlreadyInFlight,
            ScriptedOutcome::Enqueued,
        ]);
        let mine = batch("X");
        let someone_else = batch("Y").owner;

        let queued = pool
            .enqueue_next(0, &backend, &mine)
            .await
            .expect("enqueued");

        assert_eq!(queued.source, "peer_c");
        let registry = TransferRegistry::global();
        assert!(
            registry.may_adopt(&key(&backend, "peer_b", "b.flac"), &someone_else),
            "the refused alternate's key was released"
        );
        assert!(registry.may_adopt(&key(&backend, "peer_c", "c.flac"), &mine.owner));
        assert!(
            !registry.may_adopt(&key(&backend, "peer_c", "c.flac"), &someone_else),
            "the accepted alternate stays claimed by its batch"
        );
    }

    #[tokio::test]
    async fn an_alternate_another_user_is_downloading_is_skipped() {
        let items = vec![
            item("peer_a", "Wake Up", "a.flac"),
            item("peer_b", "Wake Up", "b.flac"),
            item("peer_c", "Wake Up", "c.flac"),
        ];
        let mut pool = SourcePool::single(items);
        let stub = StubBackend::new(vec![ScriptedOutcome::Enqueued]);
        let backend: Arc<dyn DownloadBackend> = stub.clone();
        let other = batch("Other");
        TransferRegistry::global().claim(&[key(&backend, "peer_b", "b.flac")], &other);

        let queued = pool
            .enqueue_next(0, &backend, &batch("X"))
            .await
            .expect("enqueued");

        assert_eq!(queued.source, "peer_c");
        assert_eq!(stub.destinations().len(), 1, "peer_b was never asked");
        assert!(
            TransferRegistry::global().may_adopt(&key(&backend, "peer_b", "b.flac"), &other.owner)
        );
    }

    #[tokio::test]
    async fn returns_none_when_every_alternate_fails() {
        let items = vec![
            item("peer_a", "Wake Up", "a.flac"),
            item("peer_b", "Wake Up", "b.flac"),
        ];
        let mut pool = SourcePool::single(items);
        let backend: Arc<dyn DownloadBackend> = StubBackend::new(vec![ScriptedOutcome::Rejected]);

        assert!(pool.enqueue_next(0, &backend, &batch("X")).await.is_none());
    }

    #[tokio::test]
    async fn stops_enqueueing_once_the_budget_is_spent() {
        // 1 already-used source plus 6 alternates: more alternates than the
        // budget (MAX_SOURCES_PER_TRACK - 1 = 5) allows.
        let items: Vec<DownloadableItem> = (0..7)
            .map(|i| item(&format!("peer_{i}"), "Wake Up", &format!("{i}.flac")))
            .collect();
        let mut pool = SourcePool::single(items);
        // Every alternate the backend is asked about is rejected. A
        // regression that drained `alternates` directly instead of going
        // through `take_next` would call the backend a 6th time here.
        let stub = StubBackend::new(vec![]);
        let backend: Arc<dyn DownloadBackend> = stub.clone();

        assert!(pool.enqueue_next(0, &backend, &batch("X")).await.is_none());
        assert_eq!(pool.attempts_used(0), MAX_SOURCES_PER_TRACK);
        assert_eq!(stub.destinations().len(), MAX_SOURCES_PER_TRACK - 1);
    }

    #[test]
    fn the_budget_is_six_sources_per_track() {
        assert_eq!(MAX_SOURCES_PER_TRACK, 6);
    }

    #[test]
    fn title_key_ignores_numbering_features_and_remasters() {
        for (a, b) in [
            ("Song (feat. X)", "song"),
            ("Song [feat X]", "Song"),
            ("Song ft. X", "Song"),
            ("Song featuring X & Y", "Song"),
            ("Song - 2011 Remaster", "Song"),
            ("Song (Remastered 2009)", "Song"),
            ("Song (2011 Remaster)", "Song"),
            ("03 - Song", "Song"),
            ("03. Song", "Song"),
            ("3 - Song", "Song"),
            ("Song (Radio Edit)", "Song"),
            ("Song (Album Version)", "Song"),
            ("07 - Wake Up (feat. X) [2011 Remaster]", "Wake Up"),
            ("Don't  Stop!", "dont stop"),
            ("Wake Up ", "wake up"),
        ] {
            assert_eq!(title_key(a), title_key(b), "{a:?} should match {b:?}");
        }
    }

    #[test]
    fn title_key_keeps_other_recordings_apart() {
        for (a, b) in [
            ("Song (Live)", "Song"),
            ("Song [Live]", "Song"),
            ("Song - Live", "Song"),
            ("Song (Live Version)", "Song"),
            ("Song (Remix)", "Song"),
            ("Song (Kowloon Remix)", "Song"),
            ("Song (Acoustic)", "Song"),
            ("Song (Acoustic Version)", "Song"),
            ("Song (Instrumental)", "Song"),
            ("Wake Up", "Come Over"),
        ] {
            assert_ne!(title_key(a), title_key(b), "{a:?} must not match {b:?}");
        }
        // A title that is only a number is not a track number.
        assert_eq!(title_key("1999"), "1999");
    }

    #[test]
    fn from_tracks_matches_alternates_by_fuzzy_title() {
        let tracks = vec![Some(item("peer_a", "Wake Up", "a.flac"))];
        let rest = vec![group(
            "peer_b",
            vec![
                item("peer_b", "Wake Up (Live)", "b-live.flac"),
                item("peer_b", "07 - Wake Up (feat. X)", "b.flac"),
            ],
            0.9,
        )];

        let mut pool = SourcePool::from_tracks(&tracks, &rest);
        assert_eq!(pool.take_next(0).map(|i| i.id), Some("b.flac".to_string()));
        assert!(
            pool.take_next(0).is_none(),
            "the live recording is no alternate"
        );
    }

    /// A pool for one "Wake Up" from peer_a whose static alternates come
    /// from `statics`, re-searching through `finder`.
    fn researching_pool(statics: &[&str], finder: &Arc<ScriptedFinder>) -> SourcePool {
        let mut items = vec![item("peer_a", "Wake Up", "a.flac")];
        items.extend(
            statics
                .iter()
                .map(|peer| item(peer, "Wake Up", &format!("{peer}.flac"))),
        );
        SourcePool::single(items).with_finder(finder.clone())
    }

    #[tokio::test]
    async fn researches_once_the_static_alternates_run_out() {
        let finder = ScriptedFinder::new(vec![vec![group(
            "peer_z",
            vec![item("peer_z", "Wake Up", "z.flac")],
            0.9,
        )]]);
        let mut pool = researching_pool(&["peer_b"], &finder);
        let backend: Arc<dyn DownloadBackend> =
            StubBackend::new(vec![ScriptedOutcome::Enqueued, ScriptedOutcome::Enqueued]);

        let first = pool.enqueue_next(0, &backend, &batch("X")).await;
        assert_eq!(first.map(|q| q.source), Some("peer_b".to_string()));
        assert!(finder.queries().is_empty(), "static alternates come first");

        let second = pool.enqueue_next(0, &backend, &batch("X")).await;
        assert_eq!(second.map(|q| q.source), Some("peer_z".to_string()));
        assert_eq!(finder.queries(), vec!["Kowloon Wake Up".to_string()]);
    }

    #[tokio::test]
    async fn a_slot_without_static_alternates_researches() {
        let finder = ScriptedFinder::new(vec![vec![group(
            "peer_z",
            vec![item("peer_z", "Wake Up", "z.flac")],
            0.9,
        )]]);
        let tracks = vec![Some(item("peer_a", "Wake Up", "a.flac"))];
        let mut pool = SourcePool::from_tracks(&tracks, &[]).with_finder(finder.clone());
        let backend: Arc<dyn DownloadBackend> = StubBackend::new(vec![ScriptedOutcome::Enqueued]);

        let queued = pool.enqueue_next(0, &backend, &batch("X")).await;
        assert_eq!(queued.map(|q| q.source), Some("peer_z".to_string()));
    }

    #[tokio::test]
    async fn the_title_alone_is_searched_only_when_the_first_query_finds_nothing() {
        // The first query only turns up another song and a live take.
        let finder = ScriptedFinder::new(vec![
            vec![group(
                "peer_y",
                vec![
                    item("peer_y", "Come Over", "y-come-over.flac"),
                    item("peer_y", "Wake Up (Live)", "y-live.flac"),
                ],
                0.9,
            )],
            vec![group(
                "peer_z",
                vec![item("peer_z", "Wake Up", "z.flac")],
                0.9,
            )],
        ]);
        let mut pool = researching_pool(&[], &finder);
        let backend: Arc<dyn DownloadBackend> = StubBackend::new(vec![ScriptedOutcome::Enqueued]);

        let queued = pool.enqueue_next(0, &backend, &batch("X")).await;
        assert_eq!(queued.map(|q| q.id), Some("t-z.flac".to_string()));
        assert_eq!(
            finder.queries(),
            vec!["Kowloon Wake Up".to_string(), "Wake Up".to_string()]
        );
    }

    #[tokio::test]
    async fn the_title_alone_is_not_searched_when_the_first_query_finds_one() {
        let finder = ScriptedFinder::new(vec![vec![group(
            "peer_z",
            vec![item("peer_z", "Wake Up", "z.flac")],
            0.9,
        )]]);
        let mut pool = researching_pool(&[], &finder);
        // The re-searched source refuses, and nothing comes after it.
        let backend: Arc<dyn DownloadBackend> = StubBackend::new(vec![ScriptedOutcome::Rejected]);

        assert!(pool.enqueue_next(0, &backend, &batch("X")).await.is_none());
        assert!(pool.enqueue_next(0, &backend, &batch("X")).await.is_none());
        assert_eq!(
            finder.queries(),
            vec!["Kowloon Wake Up".to_string()],
            "no second re-search inside the cooldown"
        );
    }

    #[tokio::test]
    async fn research_skips_the_sources_already_tried() {
        let finder = ScriptedFinder::new(vec![vec![group(
            "mixed",
            vec![
                item("peer_a", "Wake Up", "a.flac"),
                item("peer_b", "Wake Up", "peer_b.flac"),
                item("peer_c", "Wake Up", "c.flac"),
            ],
            0.9,
        )]]);
        let mut pool = researching_pool(&["peer_b"], &finder);
        let stub = StubBackend::new(vec![]);
        let backend: Arc<dyn DownloadBackend> = stub.clone();

        assert!(pool.enqueue_next(0, &backend, &batch("X")).await.is_none());
        // peer_b once as a static alternate, then only peer_c from the
        // re-search: the original and peer_b were already tried.
        assert_eq!(stub.destinations().len(), 2);
        assert_eq!(pool.attempts_used(0), 3);
    }

    #[tokio::test]
    async fn research_drops_candidates_whose_duration_is_off() {
        let timed = |peer: &str, secs: u32| DownloadableItem {
            duration: Some(secs),
            ..item(peer, "Wake Up", &format!("{peer}.flac"))
        };
        // The original runs 200s, so 10s is the larger tolerance.
        let finder = ScriptedFinder::new(vec![vec![group(
            "mixed",
            vec![timed("peer_long", 230), timed("peer_close", 209)],
            0.9,
        )]]);
        let mut pool = researching_pool(&[], &finder);
        let backend: Arc<dyn DownloadBackend> = StubBackend::new(vec![ScriptedOutcome::Enqueued]);

        let queued = pool.enqueue_next(0, &backend, &batch("X")).await;
        assert_eq!(queued.map(|q| q.source), Some("peer_close".to_string()));

        // A 600s original allows 5%, 30s.
        let original = DownloadableItem {
            duration: Some(600),
            ..item("peer_a", "Wake Up", "a.flac")
        };
        let finder = ScriptedFinder::new(vec![vec![group(
            "mixed",
            vec![timed("peer_far", 640), timed("peer_near", 625)],
            0.9,
        )]]);
        let mut pool = SourcePool::single(vec![original]).with_finder(finder);
        let stub = StubBackend::new(vec![]);
        let backend: Arc<dyn DownloadBackend> = stub.clone();

        assert!(pool.enqueue_next(0, &backend, &batch("X")).await.is_none());
        assert_eq!(stub.destinations().len(), 1, "only peer_near was asked");
    }

    #[tokio::test]
    async fn researched_candidates_are_tried_best_score_first() {
        let finder = ScriptedFinder::new(vec![vec![
            group(
                "peer_low",
                vec![item("peer_low", "Wake Up", "low.flac")],
                0.4,
            ),
            group(
                "peer_high",
                vec![item("peer_high", "Wake Up", "high.flac")],
                0.9,
            ),
        ]]);
        let mut pool = researching_pool(&[], &finder);
        let backend: Arc<dyn DownloadBackend> = StubBackend::new(vec![ScriptedOutcome::Enqueued]);

        let queued = pool.enqueue_next(0, &backend, &batch("X")).await;
        assert_eq!(queued.map(|q| q.source), Some("peer_high".to_string()));
    }

    #[tokio::test]
    async fn researched_sources_count_against_the_budget() {
        let finder = ScriptedFinder::new(vec![vec![group(
            "many",
            (0..8)
                .map(|i| item(&format!("peer_r{i}"), "Wake Up", &format!("r{i}.flac")))
                .collect(),
            0.9,
        )]]);
        let mut pool = researching_pool(&["peer_b", "peer_c"], &finder);
        let stub = StubBackend::new(vec![]);
        let backend: Arc<dyn DownloadBackend> = stub.clone();

        assert!(pool.enqueue_next(0, &backend, &batch("X")).await.is_none());
        // Two static alternates and three re-searched ones after the original.
        assert_eq!(stub.destinations().len(), 5);
        assert_eq!(pool.attempts_used(0), MAX_SOURCES_PER_TRACK);
    }

    #[test]
    fn query_title_drops_numbering_features_and_remasters_only() {
        for (raw, query) in [
            ("03 - Song (feat. X) [2011 Remaster]", "Song"),
            ("07. Wake Up", "Wake Up"),
            ("3 - Song", "Song"),
            ("Song ft. X", "Song"),
            ("Song Feat. X & Y", "Song"),
            ("Song featuring X", "Song"),
            ("Song - 2011 Remaster", "Song"),
            ("Song \u{2013} Remastered 2009", "Song"),
            ("Song (Remastered 2009)", "Song"),
            ("Song (Radio Edit)", "Song"),
            ("Song [Album Version]", "Song"),
            ("  Wake   Up  ", "Wake Up"),
            ("Don't Stop Me Now!", "Don't Stop Me Now!"),
            ("Song (Live)", "Song (Live)"),
            ("Song [Live at Wembley]", "Song [Live at Wembley]"),
            ("Song - Live", "Song - Live"),
            ("Song (Kowloon Remix)", "Song (Kowloon Remix)"),
            ("Song (Acoustic Version)", "Song (Acoustic Version)"),
            ("Song (Instrumental) (feat. X)", "Song (Instrumental)"),
            ("1999", "1999"),
            // Cleaning would leave nothing, so the raw title goes out.
            ("(feat. X)", "(feat. X)"),
            (" [2011 Remaster] ", "[2011 Remaster]"),
        ] {
            assert_eq!(query_title(raw), query, "query for {raw:?}");
        }
    }

    #[tokio::test]
    async fn research_sends_the_cleaned_title() {
        let finder = ScriptedFinder::new(vec![]);
        let original = item("peer_a", "07 - Wake Up (feat. X) [2011 Remaster]", "a.flac");
        let mut pool = SourcePool::single(vec![original]).with_finder(finder.clone());
        let backend: Arc<dyn DownloadBackend> = StubBackend::new(vec![]);

        assert!(pool.enqueue_next(0, &backend, &batch("X")).await.is_none());
        assert_eq!(
            finder.queries(),
            vec!["Kowloon Wake Up".to_string(), "Wake Up".to_string()]
        );
    }

    #[test]
    fn the_research_budget_is_three_five_minutes_apart() {
        assert_eq!(MAX_RESEARCHES_PER_SLOT, 3);
        assert_eq!(RESEARCH_COOLDOWN, Duration::from_secs(5 * 60));
    }

    #[tokio::test]
    async fn a_fruitless_research_may_run_again_after_the_cooldown() {
        // The first re-search, both queries, finds nothing: its peers are
        // offline. They come back for the third query.
        let finder = ScriptedFinder::new(vec![
            vec![],
            vec![],
            vec![group(
                "peer_z",
                vec![item("peer_z", "Wake Up", "z.flac")],
                0.9,
            )],
        ]);
        let mut pool = researching_pool(&[], &finder);
        let backend: Arc<dyn DownloadBackend> = StubBackend::new(vec![ScriptedOutcome::Enqueued]);

        assert!(pool.enqueue_next(0, &backend, &batch("X")).await.is_none());
        assert_eq!(finder.queries().len(), 2);

        // Not yet: the cooldown has not run out.
        pool.backdate_research(0, RESEARCH_COOLDOWN - Duration::from_secs(5));
        assert!(pool.enqueue_next(0, &backend, &batch("X")).await.is_none());
        assert_eq!(
            finder.queries().len(),
            2,
            "searched again inside the cooldown"
        );

        pool.backdate_research(0, RESEARCH_COOLDOWN + Duration::from_secs(1));
        let queued = pool.enqueue_next(0, &backend, &batch("X")).await;
        assert_eq!(queued.map(|q| q.source), Some("peer_z".to_string()));
        assert_eq!(finder.queries().len(), 3);
    }

    #[tokio::test]
    async fn a_slot_researches_at_most_three_times() {
        let finder = ScriptedFinder::new(vec![]);
        let mut pool = researching_pool(&[], &finder);
        let backend: Arc<dyn DownloadBackend> = StubBackend::new(vec![]);

        for round in 1..=MAX_RESEARCHES_PER_SLOT + 2 {
            assert!(pool.enqueue_next(0, &backend, &batch("X")).await.is_none());
            pool.backdate_research(0, RESEARCH_COOLDOWN + Duration::from_secs(1));
            let researches = round.min(MAX_RESEARCHES_PER_SLOT);
            assert_eq!(
                finder.queries().len(),
                2 * researches,
                "after {round} failovers"
            );
        }
    }

    #[tokio::test]
    async fn a_later_research_skips_what_an_earlier_one_offered() {
        let finder = ScriptedFinder::new(vec![
            vec![group(
                "peer_y",
                vec![item("peer_y", "Wake Up", "y.flac")],
                0.9,
            )],
            vec![group(
                "mixed",
                vec![
                    item("peer_y", "Wake Up", "y.flac"),
                    item("peer_z", "Wake Up", "z.flac"),
                ],
                0.9,
            )],
        ]);
        let mut pool = researching_pool(&[], &finder);
        // peer_y refuses; peer_z, from the second re-search, accepts.
        let stub = StubBackend::new(vec![ScriptedOutcome::Rejected, ScriptedOutcome::Enqueued]);
        let backend: Arc<dyn DownloadBackend> = stub.clone();

        assert!(pool.enqueue_next(0, &backend, &batch("X")).await.is_none());
        pool.backdate_research(0, RESEARCH_COOLDOWN + Duration::from_secs(1));
        let queued = pool.enqueue_next(0, &backend, &batch("X")).await;

        assert_eq!(queued.map(|q| q.source), Some("peer_z".to_string()));
        assert_eq!(stub.destinations().len(), 2, "peer_y was not asked twice");
        assert_eq!(pool.attempts_used(0), 3);
    }

    /// `item` by `artist` instead of the fixture's Kowloon.
    fn by(artist: &str, peer: &str, title: &str, id: &str) -> DownloadableItem {
        DownloadableItem {
            artist: artist.to_string(),
            ..item(peer, title, id)
        }
    }

    #[tokio::test]
    async fn research_rejects_another_artists_song_on_both_queries() {
        // Another artist's "Intro", the same length as Kowloon's, on either
        // query: the title-only one widens the search, not what passes.
        let finder = ScriptedFinder::new(vec![
            vec![group(
                "peer_y",
                vec![by(
                    "Someone Else",
                    "peer_y",
                    "Intro",
                    "Someone Else\\Debut\\01 - Intro.flac",
                )],
                0.9,
            )],
            vec![group(
                "peer_z",
                vec![by(
                    "Other Band",
                    "peer_z",
                    "Intro",
                    "Other Band\\Live\\01 Intro.flac",
                )],
                0.9,
            )],
        ]);
        let original = item("peer_a", "Intro", "a.flac");
        let mut pool = SourcePool::single(vec![original]).with_finder(finder.clone());
        let stub = StubBackend::new(vec![ScriptedOutcome::Enqueued]);
        let backend: Arc<dyn DownloadBackend> = stub.clone();

        assert!(pool.enqueue_next(0, &backend, &batch("X")).await.is_none());
        assert_eq!(finder.queries(), vec!["Kowloon Intro", "Intro"]);
        assert!(
            stub.destinations().is_empty(),
            "no other artist was enqueued"
        );
    }

    #[test]
    fn a_candidate_artist_matches_by_name_or_path() {
        let original = DownloadableItem {
            artist: "The Kowloon & Friends".to_string(),
            ..item("peer_a", "Wake Up", "a.flac")
        };
        let groups = vec![group(
            "mixed",
            vec![
                // Named, with "The" and "&" spelled differently.
                by("Kowloon and Friends", "peer_b", "Wake Up", "b.flac"),
                // Named with its significant words in another order.
                by("Friends & Kowloon, The", "peer_c", "Wake Up", "c.flac"),
                // No artist, but the folder names it.
                by(
                    "",
                    "peer_d",
                    "Wake Up",
                    "Music\\Kowloon & Friends\\Come Over\\07 - Wake Up.flac",
                ),
                by(
                    "Unknown Artist",
                    "peer_e",
                    "Wake Up",
                    "share/the_kowloon_and_friends/07.flac",
                ),
                // No artist, and nothing in the path says whose it is.
                by("", "peer_f", "Wake Up", "Music\\Misc\\07 - Wake Up.flac"),
                // Somebody else, or a name that only shares a word.
                by(
                    "Someone Else",
                    "peer_g",
                    "Wake Up",
                    "Someone Else\\07 - Wake Up.flac",
                ),
                by("Kowloon Tong", "peer_h", "Wake Up", "h.flac"),
            ],
            0.9,
        )];

        let sources: Vec<String> = usable_candidates(&original, groups, &[])
            .into_iter()
            .map(|c| c.source)
            .collect();
        assert_eq!(sources, vec!["peer_b", "peer_c", "peer_d", "peer_e"]);
    }

    #[test]
    fn a_candidate_of_unknown_length_ranks_after_the_timed_ones() {
        let untimed = DownloadableItem {
            duration: None,
            ..item("peer_unknown", "Wake Up", "u.flac")
        };
        let timed = DownloadableItem {
            duration: Some(205),
            ..item("peer_timed", "Wake Up", "t.flac")
        };
        let groups = vec![
            group("peer_unknown", vec![untimed], 0.9),
            group("peer_timed", vec![timed], 0.5),
        ];

        let original = item("peer_a", "Wake Up", "a.flac");
        let sources: Vec<String> = usable_candidates(&original, groups, &[])
            .into_iter()
            .map(|c| c.source)
            .collect();
        assert_eq!(sources, vec!["peer_timed", "peer_unknown"]);
    }

    #[test]
    fn a_leading_number_is_a_track_number_only_when_marked_as_one() {
        for title in [
            "99 Problems",
            "7 Rings",
            "21 Guns",
            "2 Become 1",
            "1.5 Miles",
            "1999",
        ] {
            assert_eq!(
                title_key(title),
                shared::recommendation::normalize_for_matching(title),
                "{title:?} keeps its number"
            );
            assert_eq!(query_title(title), title, "query for {title:?}");
        }
        for numbered in [
            "03 - Song",
            "03. Song",
            "03_Song",
            "03) Song",
            "3 - Song",
            "03 Song",
            "007 Song",
        ] {
            assert_eq!(title_key(numbered), "song", "key for {numbered:?}");
            assert_eq!(query_title(numbered), "Song", "query for {numbered:?}");
        }
    }

    #[test]
    fn only_known_qualifiers_of_the_same_recording_are_dropped() {
        for kept in [
            "Song (Karaoke Version)",
            "Song (Demo Version)",
            "Song (Cover Version)",
            "Song (Piano Version)",
            "Song (Spanish Version)",
            "Song - Demo Version",
        ] {
            assert_ne!(
                title_key(kept),
                title_key("Song"),
                "{kept:?} is another recording"
            );
            assert_eq!(query_title(kept), kept, "query for {kept:?}");
        }
        for dropped in [
            "Song (Remastered)",
            "Song (2011 Remaster)",
            "Song - Remastered 2009",
            "Song - Radio Edit",
            "Song (feat. X)",
            "Song (Single Version)",
            "Song (Album Version)",
            "Song (Original Version)",
            "Song (Explicit)",
            "Song [Clean]",
            "Song (Mono)",
            "Song (Stereo)",
            "Song [Deluxe]",
            "Song (Bonus Track)",
            "Song (2011)",
        ] {
            assert_eq!(title_key(dropped), "song", "key for {dropped:?}");
            assert_eq!(query_title(dropped), "Song", "query for {dropped:?}");
        }
    }

    #[test]
    fn an_empty_title_key_matches_nothing() {
        let tracks = vec![
            Some(item("peer_a", "(feat. X)", "a.flac")),
            Some(item("peer_a", "", "b.flac")),
        ];
        let rest = vec![group(
            "peer_b",
            vec![
                item("peer_b", "[2011 Remaster]", "x.flac"),
                item("peer_b", "", "y.flac"),
            ],
            0.9,
        )];

        let mut pool = SourcePool::from_tracks(&tracks, &rest);
        assert!(pool.take_next(0).is_none());
        assert!(pool.take_next(1).is_none());

        let groups = vec![group("peer_b", vec![item("peer_b", "", "y.flac")], 0.9)];
        assert!(usable_candidates(&item("peer_a", "", "b.flac"), groups, &[]).is_empty());
    }

    /// Wait until `finder` has been asked `count` queries.
    async fn queries_reach(finder: &ScriptedFinder, count: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while finder.queries().len() < count {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("never reached {count} queries: {:?}", finder.queries()));
    }

    #[tokio::test]
    async fn research_waits_for_a_free_permit() {
        let titles = ["One", "Two", "Three"];
        let answer = || {
            vec![group(
                "peer_z",
                titles
                    .iter()
                    .map(|t| item("peer_z", t, &format!("z-{t}.flac")))
                    .collect(),
                0.9,
            )]
        };
        let finder = ScriptedFinder::new(vec![answer(), answer(), answer()]);
        let (_reached, release) = finder.hold_searches();
        let tracks: Vec<_> = titles
            .iter()
            .map(|t| Some(item("peer_a", t, &format!("a-{t}.flac"))))
            .collect();
        let mut pool = SourcePool::from_tracks(&tracks, &[])
            .with_finder(finder.clone())
            .with_research_limit(2);
        let backend: Arc<dyn DownloadBackend> = StubBackend::new(vec![]);
        let batch = batch("X");

        for slot in 0..titles.len() {
            assert!(matches!(
                pool.enqueue_or_search(slot, &backend, &batch, Trigger::Failure)
                    .await,
                Failover::Searching
            ));
        }
        queries_reach(&finder, 2).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(finder.queries().len(), 2, "a third search ran beside two");
        for slot in 0..titles.len() {
            assert!(
                matches!(
                    pool.enqueue_or_search(slot, &backend, &batch, Trigger::Failure)
                        .await,
                    Failover::Searching
                ),
                "slot {slot} is searching or waiting to"
            );
        }

        // One search answers and frees its permit for the third.
        release.notify_one();
        queries_reach(&finder, 3).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(finder.queries().len(), 3);

        release.notify_one();
        release.notify_one();
        for slot in 0..titles.len() {
            pool.research_settled(slot).await;
        }
    }

    #[tokio::test]
    async fn an_errored_research_costs_no_research_but_waits_the_cooldown() {
        let finder = ScriptedFinder::new(vec![]);
        finder.fail_searches(1);
        let mut pool = researching_pool(&[], &finder);
        let backend: Arc<dyn DownloadBackend> = StubBackend::new(vec![]);

        // The search errors on its first query, so the second is not sent.
        assert!(pool.enqueue_next(0, &backend, &batch("X")).await.is_none());
        assert_eq!(finder.queries().len(), 1);
        assert!(pool.enqueue_next(0, &backend, &batch("X")).await.is_none());
        assert_eq!(
            finder.queries().len(),
            1,
            "searched again inside the cooldown"
        );

        // The whole research budget is still there.
        for round in 1..=MAX_RESEARCHES_PER_SLOT + 1 {
            pool.backdate_research(0, RESEARCH_COOLDOWN + Duration::from_secs(1));
            assert!(pool.enqueue_next(0, &backend, &batch("X")).await.is_none());
            assert_eq!(
                finder.queries().len(),
                1 + 2 * round.min(MAX_RESEARCHES_PER_SLOT),
                "after {round} more failovers"
            );
        }
    }

    #[tokio::test]
    async fn an_errored_background_research_costs_no_research() {
        let finder = ScriptedFinder::new(vec![]);
        finder.fail_searches(1);
        let mut pool = researching_pool(&[], &finder);
        let backend: Arc<dyn DownloadBackend> = StubBackend::new(vec![]);
        let batch = batch("X");

        assert!(matches!(
            pool.enqueue_or_search(0, &backend, &batch, Trigger::Failure)
                .await,
            Failover::Searching
        ));
        pool.research_settled(0).await;
        for round in 1..=MAX_RESEARCHES_PER_SLOT + 1 {
            pool.backdate_research(0, RESEARCH_COOLDOWN + Duration::from_secs(1));
            pool.enqueue_or_search(0, &backend, &batch, Trigger::Failure)
                .await;
            pool.research_settled(0).await;
            assert_eq!(
                finder.queries().len(),
                1 + 2 * round.min(MAX_RESEARCHES_PER_SLOT),
                "after {round} more failovers"
            );
        }
    }

    #[tokio::test]
    async fn a_failure_after_a_fruitless_stall_search_searches_inside_the_cooldown() {
        let finder = ScriptedFinder::new(vec![
            vec![],
            vec![],
            vec![group(
                "peer_z",
                vec![item("peer_z", "Wake Up", "z.flac")],
                0.9,
            )],
        ]);
        let mut pool = researching_pool(&[], &finder);
        let backend: Arc<dyn DownloadBackend> = StubBackend::new(vec![ScriptedOutcome::Enqueued]);
        let batch = batch("X");

        assert!(matches!(
            pool.enqueue_or_search(0, &backend, &batch, Trigger::Stall)
                .await,
            Failover::Searching
        ));
        pool.research_settled(0).await;
        assert!(
            matches!(
                pool.enqueue_or_search(0, &backend, &batch, Trigger::Stall)
                    .await,
                Failover::Exhausted
            ),
            "another stall waits out the cooldown"
        );
        assert_eq!(finder.queries().len(), 2);

        assert!(matches!(
            pool.enqueue_or_search(0, &backend, &batch, Trigger::Failure)
                .await,
            Failover::Searching
        ));
        pool.research_settled(0).await;
        let Failover::Enqueued(queued) = pool
            .enqueue_or_search(0, &backend, &batch, Trigger::Failure)
            .await
        else {
            panic!("the failure's search found peer_z");
        };
        assert_eq!(queued.source, "peer_z");
        assert_eq!(finder.queries().len(), 3);

        // That failure's search spent the exception: the next one waits.
        assert!(matches!(
            pool.enqueue_or_search(0, &backend, &batch, Trigger::Failure)
                .await,
            Failover::Exhausted
        ));
        assert_eq!(finder.queries().len(), 3);
    }
}
