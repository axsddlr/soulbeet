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
use soulbeet::DownloadBackend;
use std::collections::VecDeque;
use std::sync::Arc;
use tracing::{info, warn};

use super::enqueue::{enqueue_claimed, transfer_key};
use crate::transfer_registry::{BatchRef, TransferRegistry};

/// Sources tried per track before it is declared failed: the peer picked at
/// search time plus five alternates, from the original search or a fresh
/// one. slskd retries each peer 3 times on its own (~40s) before reporting
/// failure, so this caps a dead track at roughly 4 minutes plus the
/// re-search.
pub const MAX_SOURCES_PER_TRACK: usize = 6;

/// How far a re-searched candidate's duration may stray from the original's,
/// at least, before it is taken for a different recording.
const DURATION_TOLERANCE_SECS: f64 = 10.0;

/// The same tolerance as a share of the original's duration, used when it is
/// the larger of the two.
const DURATION_TOLERANCE_SHARE: f64 = 0.05;

/// Searches for one track again once its alternates are spent. Returns the
/// search's result groups unfiltered: the pool keeps the ones that are the
/// same track. An empty `artist` searches the title alone.
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
    ) -> Vec<DownloadableGroup>;
}

/// Qualifier words that name a different recording than the bare title, so
/// a qualifier holding one stays part of the key.
fn names_another_recording(word: &str) -> bool {
    matches!(word, "live" | "acoustic" | "instrumental") || word.starts_with("remix")
}

/// Whether a bracketed or dashed qualifier only describes the same
/// recording: a featured artist, a remaster, an edit or a version.
fn is_dropped_qualifier(qualifier: &str) -> bool {
    let words: Vec<&str> = qualifier
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    match words.first() {
        None => return true,
        Some(&("feat" | "ft" | "featuring")) => return true,
        Some(_) => {}
    }
    if words.iter().any(|w| names_another_recording(w)) {
        return false;
    }
    words.iter().any(|w| w.starts_with("remaster"))
        || matches!(words.last(), Some(&("version" | "edit")))
}

/// `title` without a leading track number ("03 - ", "03. ", "3 "). A title
/// that is nothing but a number keeps it.
fn strip_track_number(title: &str) -> &str {
    let title = title.trim_start();
    let digits = title.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 || digits > 3 {
        return title;
    }
    let after = &title[digits..];
    let spaced = after.trim_start();
    let (separated, rest) = match spaced.strip_prefix(['-', '.']) {
        Some(rest) => (true, rest),
        None => (spaced.len() != after.len(), spaced),
    };
    let rest = rest.trim_start();
    if !separated || !rest.chars().any(char::is_alphanumeric) {
        return title;
    }
    rest
}

/// `segment` up to a featured-artist marker ("ft.", "feat", "featuring").
fn strip_featured(segment: &str) -> String {
    segment
        .split_whitespace()
        .enumerate()
        .take_while(|(i, word)| {
            *i == 0 || !matches!(*word, "feat" | "feat." | "ft" | "ft." | "featuring")
        })
        .map(|(_, word)| word)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The key two titles of the same recording share: lowercased, without a
/// leading track number, featured artists, remaster, edit or version
/// qualifiers, and with punctuation and whitespace collapsed. Live, remix,
/// acoustic and instrumental qualifiers stay, since those are different
/// recordings.
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

/// Whether a candidate's duration is close enough to the original's to be
/// the same recording. Unknown on either side cannot be judged, so it passes.
fn duration_matches(original: Option<u32>, candidate: Option<u32>) -> bool {
    let (Some(original), Some(candidate)) = (original, candidate) else {
        return true;
    };
    let tolerance = DURATION_TOLERANCE_SECS.max(f64::from(original) * DURATION_TOLERANCE_SHARE);
    (f64::from(original) - f64::from(candidate)).abs() <= tolerance
}

/// The items of a re-search that can stand in for `track`, best group
/// first: the same title key, a close duration, and a source not `tried`.
fn usable_candidates(
    track: &DownloadableItem,
    mut groups: Vec<DownloadableGroup>,
    tried: &[(String, String)],
) -> Vec<DownloadableItem> {
    groups.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let wanted = title_key(&track.title);
    let mut found: Vec<DownloadableItem> = Vec::new();
    for candidate in groups.into_iter().flat_map(|group| group.items) {
        let seen = |source: &str, id: &str| source == candidate.source && id == candidate.id;
        if title_key(&candidate.title) == wanted
            && duration_matches(track.duration, candidate.duration)
            && !tried.iter().any(|(source, id)| seen(source, id))
            && !found.iter().any(|f| seen(&f.source, &f.id))
        {
            found.push(candidate);
        }
    }
    found
}

/// Search for `track` again: by artist and title, then by the title alone
/// only when that finds nothing usable.
async fn research(
    finder: &dyn AlternateFinder,
    backend: &Arc<dyn DownloadBackend>,
    track: &DownloadableItem,
    tried: &[(String, String)],
) -> Vec<DownloadableItem> {
    let title = track.title.trim();
    let artist = track.artist.trim();
    let mut queries = Vec::new();
    if !artist.is_empty() {
        queries.push(artist);
    }
    queries.push("");
    for artist in queries {
        let groups = finder.search(backend, artist, title).await;
        let found = usable_candidates(track, groups, tried);
        info!(
            "Re-search for '{} {}' found {} new source(s)",
            artist,
            title,
            found.len()
        );
        if !found.is_empty() {
            return found;
        }
    }
    Vec::new()
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
    /// Whether this slot already re-searched. Each query runs once per slot.
    researched: bool,
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
            researched: false,
            used: 1,
        }
    }
}

/// Ranked alternate sources for every track in a batch, indexed by slot.
pub struct SourcePool {
    slots: Vec<Slot>,
    /// Searches again for a slot whose alternates ran out; `None` disables
    /// re-searching.
    finder: Option<Arc<dyn AlternateFinder>>,
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
                    Some(track) => {
                        let wanted = title_key(&track.title);
                        rest.iter()
                            .flat_map(|group| group.items.iter())
                            .filter(|candidate| title_key(&candidate.title) == wanted)
                            .cloned()
                            .collect()
                    }
                    None => VecDeque::new(),
                };
                Slot::new(track.clone(), alternates)
            })
            .collect();

        Self {
            slots,
            finder: None,
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
        }
    }

    /// Search again for a slot whose alternates ran out, or that had none.
    pub fn with_finder(mut self, finder: Arc<dyn AlternateFinder>) -> Self {
        self.finder = Some(finder);
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

    /// `take_next`, re-searching once on `backend` when the slot's
    /// alternates are spent but its budget is not.
    async fn next_source(
        &mut self,
        slot: usize,
        backend: &Arc<dyn DownloadBackend>,
    ) -> Option<DownloadableItem> {
        if let Some(item) = self.take_next(slot) {
            return Some(item);
        }
        let finder = self.finder.clone()?;
        let entry = self.slots.get_mut(slot)?;
        if entry.researched || entry.used >= MAX_SOURCES_PER_TRACK {
            return None;
        }
        entry.researched = true;
        let track = entry.track.clone()?;
        let found = research(finder.as_ref(), backend, &track, &entry.tried).await;
        entry.alternates.extend(found);
        self.take_next(slot)
    }

    /// Enqueue this slot's next alternate into `batch`'s folder, walking past
    /// the ones it cannot have (another batch owns the transfer, the peer is
    /// offline, slskd is already fetching it) until one is accepted or the
    /// slot runs dry. The accepted alternate's key stays claimed by `batch`.
    /// Once the static alternates are spent the slot re-searches, once. A
    /// refused alternate still costs the slot a source, so five dead peers in
    /// a row spend the whole budget without a transfer ever being attempted.
    /// `None` means the slot is out of sources or budget.
    pub async fn enqueue_next(
        &mut self,
        slot: usize,
        backend: &Arc<dyn DownloadBackend>,
        batch: &BatchRef,
    ) -> Option<QueuedDownload> {
        while let Some(item) = self.next_source(slot, backend).await {
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
                continue;
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
        }
        None
    }
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
            ("3 Song", "Song"),
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
            "each query runs at most once per slot"
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
}
