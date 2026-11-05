use dioxus::prelude::*;
use serde::{Deserialize, Serialize};
use shared::download::DownloadQuery;

#[cfg(feature = "server")]
use shared::download::{
    AutoDownloadEvent, DownloadEvent, DownloadProgress, DownloadableGroup, EnqueueStatus,
    QueuedDownload, SearchState,
};

#[cfg(feature = "server")]
use dioxus::logger::tracing::{info, warn};
#[cfg(feature = "server")]
use std::sync::Arc;
#[cfg(feature = "server")]
use std::time::Duration;
#[cfg(feature = "server")]
use tokio::sync::broadcast;

#[cfg(feature = "server")]
use crate::globals::{get_or_create_user_channel, register_user_task, unregister_user_task};
#[cfg(feature = "server")]
use crate::models::folder::Folder;
#[cfg(feature = "server")]
use crate::models::user_settings::UserSettings;
#[cfg(feature = "server")]
use crate::services::{available_download_backends, download_backend};
#[cfg(feature = "server")]
use crate::transfer_registry::{BatchRef, TransferOwner};
#[cfg(feature = "server")]
use crate::AuthSession;

#[cfg(feature = "server")]
use super::enqueue::{
    deliver, enqueue_request, failed_reason, stopped_rows, RequestEnqueue, Watcher,
};
#[cfg(feature = "server")]
use super::failover::{align_pool_tracks, AlternateFinder, SourcePool};
#[cfg(feature = "server")]
use super::monitor::{DownloadMonitor, WatchedTransfer};
#[cfg(feature = "server")]
use shared::metadata::{Album, Track};
#[cfg(feature = "server")]
use soulbeet::error::Result as SoulResult;
#[cfg(feature = "server")]
use soulbeet::DownloadBackend;

/// Score threshold for automatic source selection (per D-04).
/// Below this, the client gets FallbackToManual with results attached.
#[cfg(feature = "server")]
const AUTO_SELECT_SCORE_THRESHOLD: f64 = 0.7;

/// Maximum time to wait for search results from a single backend.
#[cfg(feature = "server")]
const SEARCH_TIMEOUT: Duration = Duration::from_secs(120);

/// Poll interval when waiting for search results.
#[cfg(feature = "server")]
const SEARCH_POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoDownloadRequest {
    pub query: DownloadQuery,
    /// Folder ID used for persisting as default_download_folder_id (per D-01)
    pub folder_id: String,
    /// Resolved folder path for the actual download target directory
    pub folder_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AutoDownloadResult {
    /// Pipeline spawned, all further results delivered via WebSocket AutoDownloadEvent
    Accepted { batch_id: String },
    /// Error before pipeline could start (no backends, bad input, etc.)
    Error(String),
}

/// One backend's results for a search, scored by the backend, or nothing
/// when it fails or finds nothing within `SEARCH_TIMEOUT`. With
/// `require_flac`, only FLAC files are results.
#[cfg(feature = "server")]
pub(super) async fn search_backend(
    id: &str,
    backend: &Arc<dyn DownloadBackend>,
    album: Option<&Album>,
    tracks: &[Track],
    require_flac: bool,
) -> Vec<DownloadableGroup> {
    try_search_backend(id, backend, album, tracks, require_flac)
        .await
        .unwrap_or_default()
}

/// `search_backend`, but a search that could not start, or whose polls
/// errored before it returned anything, is an `Err` rather than nothing. A
/// poll error after some results keeps those.
#[cfg(feature = "server")]
async fn try_search_backend(
    id: &str,
    backend: &Arc<dyn DownloadBackend>,
    album: Option<&Album>,
    tracks: &[Track],
    require_flac: bool,
) -> SoulResult<Vec<DownloadableGroup>> {
    let search_id = match backend.start_search(album, tracks, require_flac).await {
        Ok(sid) => sid,
        Err(e) => {
            warn!("Backend {} search start failed: {}", id, e);
            return Err(e);
        }
    };

    // Poll until results are ready or timeout. Polls that report
    // InProgress carry the groups processed so far (the backend
    // rescores the full response set each time), so keep the
    // latest non-empty batch: the final Completed poll is empty
    // when the search ends by timeout instead of result volume.
    let deadline = tokio::time::Instant::now() + SEARCH_TIMEOUT;
    let mut latest_groups = Vec::<DownloadableGroup>::new();
    loop {
        if tokio::time::Instant::now() >= deadline {
            warn!("Backend {} search timed out", id);
            break;
        }
        tokio::time::sleep(SEARCH_POLL_INTERVAL).await;

        match backend.poll_search(&search_id).await {
            Ok(result) => match result.state {
                SearchState::Completed | SearchState::TimedOut => {
                    if !result.groups.is_empty() {
                        latest_groups = result.groups;
                    }
                    return Ok(latest_groups);
                }
                SearchState::NotFound => {
                    return Ok(latest_groups);
                }
                SearchState::InProgress => {
                    if !result.groups.is_empty() {
                        latest_groups = result.groups;
                    }
                }
            },
            Err(e) => {
                warn!("Backend {} poll error: {}", id, e);
                return if latest_groups.is_empty() {
                    Err(e)
                } else {
                    Ok(latest_groups)
                };
            }
        }
    }
    Ok(latest_groups)
}

/// Re-searches a failing track on the backend its batch downloads from,
/// passing on a search that could not start or answer as an error.
/// `require_flac` is the batch owner's "Require FLAC only" setting, so an
/// alternate is never a format the original search would have dropped.
#[cfg(feature = "server")]
pub(super) struct BackendFinder {
    pub require_flac: bool,
}

#[cfg(feature = "server")]
#[async_trait::async_trait]
impl AlternateFinder for BackendFinder {
    async fn search(
        &self,
        backend: &Arc<dyn DownloadBackend>,
        artist: &str,
        title: &str,
    ) -> SoulResult<Vec<DownloadableGroup>> {
        let track = Track {
            id: String::new(),
            title: title.to_string(),
            artist: artist.to_string(),
            album_id: None,
            album_title: None,
            release_date: None,
            duration: None,
            mbid: None,
            release_mbid: None,
        };
        try_search_backend(backend.id(), backend, None, &[track], self.require_flac).await
    }
}

#[post("/api/auto-download", auth: AuthSession)]
pub async fn auto_download(req: AutoDownloadRequest) -> Result<AutoDownloadResult, ServerFnError> {
    let username = auth.0.username;

    if let Some(issue) = crate::importer_health::broken_issue().await {
        return Ok(AutoDownloadResult::Error(issue.title));
    }
    let folder = match Folder::get_by_id(&req.folder_id).await {
        Ok(Some(f))
            if f.user_id == auth.0.sub
                && std::path::Path::new(&req.folder_path) == std::path::Path::new(&f.path) =>
        {
            f
        }
        Ok(_) => return Ok(AutoDownloadResult::Error("Folder not found".to_string())),
        Err(e) => {
            warn!("Folder lookup for auto-download failed: {e}");
            return Ok(AutoDownloadResult::Error("Folder not found".to_string()));
        }
    };
    // Both the search and every failover re-search follow it.
    let require_flac = match UserSettings::get(&auth.0.sub).await {
        Ok(settings) => settings.require_flac_only,
        Err(e) => {
            warn!("Settings lookup for auto-download failed: {e}");
            return Ok(AutoDownloadResult::Error(
                "Could not load your settings".to_string(),
            ));
        }
    };

    let (tx, _) = get_or_create_user_channel(&username).await;

    let mut req = req;
    if let Err(e) = crate::server_fns::search::hydrate_album_tracks(&mut req.query).await {
        return Ok(AutoDownloadResult::Error(e));
    }

    let backends = available_backends().await;

    if backends.is_empty() {
        let batch_id = uuid::Uuid::new_v4().to_string();
        let _ = tx.send(DownloadEvent::AutoDownload(AutoDownloadEvent::Failed {
            batch_id,
            error: NO_BACKENDS.to_string(),
        }));
        return Ok(AutoDownloadResult::Error(NO_BACKENDS.to_string()));
    }

    // Generate batch_id upfront so we can return it immediately (per D-10)
    let batch_id = uuid::Uuid::new_v4().to_string();
    let batch_id_for_response = batch_id.clone();

    // Spawn the entire search-score-pick-download pipeline onto a background task.
    // This avoids blocking the HTTP response during the search-poll loop (Research Pitfall 4).
    let job = AlbumDownloadJob {
        user_id: auth.0.sub,
        username,
        folder_path: folder.path,
        query: req.query,
        require_flac,
        batch_id,
        backends,
        tx,
    };
    tokio::spawn(async move {
        run_album_download(job).await;
    });

    Ok(AutoDownloadResult::Accepted {
        batch_id: batch_id_for_response,
    })
}

#[cfg(feature = "server")]
const NO_BACKENDS: &str = "No download backends available";

/// Every configured download backend that is available now (per D-06:
/// search ALL configured backends).
#[cfg(feature = "server")]
async fn available_backends() -> Vec<(String, Arc<dyn DownloadBackend>)> {
    let backend_ids: Vec<String> = available_download_backends()
        .iter()
        .map(|(id, _)| id.to_string())
        .collect();

    let mut backends = Vec::new();
    for id in &backend_ids {
        match download_backend(Some(id)).await {
            Ok(b) => backends.push((id.clone(), b)),
            Err(e) => {
                warn!("Backend {} unavailable for auto-download: {}", id, e);
            }
        }
    }
    backends
}

/// Everything an auto download needs once the caller has checked who it is
/// for: the user's folder is resolved and the query's tracks are hydrated.
#[cfg(feature = "server")]
pub(crate) struct AlbumDownloadJob {
    pub user_id: String,
    pub username: String,
    /// Target directory; created if missing.
    pub folder_path: String,
    /// The search, with its tracks already hydrated.
    pub query: DownloadQuery,
    /// The user's "Require FLAC only" setting, for the search and failover.
    pub require_flac: bool,
    /// Id of the batch the download goes into, and of its events.
    pub batch_id: String,
    /// The backends to search, all in parallel.
    pub backends: Vec<(String, Arc<dyn DownloadBackend>)>,
    /// The user's event channel. Sending with no subscriber is not an error.
    pub tx: broadcast::Sender<DownloadEvent>,
}

/// What an auto download came to by the time its files were handed to a
/// monitor. The monitor, and the failover it runs, keep going on their own
/// task after this.
#[cfg(feature = "server")]
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum AlbumDownloadOutcome {
    /// Files were enqueued into `batch_id`. `file_count` counts the newly
    /// enqueued ones, and is 0 when every file was already downloading.
    Enqueued { batch_id: String, file_count: usize },
    /// Nothing was downloaded: the best source scored below
    /// `AUTO_SELECT_SCORE_THRESHOLD`.
    BelowThreshold { best_score: Option<f64> },
    /// No backend found anything.
    NoResults,
    /// Nothing was downloaded, for this reason.
    Failed(String),
}

/// Which source an auto download takes from its merged search results.
#[cfg(feature = "server")]
#[derive(Debug)]
enum SourcePick {
    NoResults,
    /// All results, best first, the best below the threshold.
    BelowThreshold {
        groups: Vec<DownloadableGroup>,
        best_score: f64,
    },
    /// The best result, and the rest best first as failover alternates.
    Picked {
        picked: DownloadableGroup,
        rest: Vec<DownloadableGroup>,
    },
}

#[cfg(feature = "server")]
impl SourcePick {
    /// The ScoringResults figures: how many results, and the best score.
    fn scoring(&self) -> Option<(usize, f64)> {
        match self {
            SourcePick::NoResults => None,
            SourcePick::BelowThreshold { groups, best_score } => Some((groups.len(), *best_score)),
            SourcePick::Picked { picked, rest } => Some((rest.len() + 1, picked.score)),
        }
    }
}

/// Sorts the merged results by score, best first (per D-07), and picks the
/// best unless it scores below `AUTO_SELECT_SCORE_THRESHOLD` (per D-04).
#[cfg(feature = "server")]
fn pick_source(mut groups: Vec<DownloadableGroup>) -> SourcePick {
    if groups.is_empty() {
        return SourcePick::NoResults;
    }
    groups.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let best_score = groups[0].score;
    if best_score < AUTO_SELECT_SCORE_THRESHOLD {
        return SourcePick::BelowThreshold { groups, best_score };
    }
    let picked = groups.remove(0);
    SourcePick::Picked {
        picked,
        rest: groups,
    }
}

/// Why an enqueue counts as failed, if it does: it errored, was refused, or
/// queued nothing while nothing was already downloading.
#[cfg(feature = "server")]
fn enqueue_failure(
    error: Option<&dyn std::fmt::Display>,
    refusal: Option<&str>,
    enqueued: usize,
    already_downloading: bool,
) -> Option<String> {
    if let Some(e) = error {
        Some(format!("Download queue failed: {}", e))
    } else if let Some(reason) = refusal {
        Some(reason.to_string())
    } else if enqueued == 0 && !already_downloading {
        Some("All downloads failed to queue".to_string())
    } else {
        None
    }
}

/// Runs an auto download: searches every backend, picks the best source or
/// falls back to manual, enqueues it and starts its monitor, sending the
/// same events throughout as the auto-download endpoint. Returns once the
/// files are enqueued (and handed to a running batch, or to a monitor
/// running on its own task), or once it is clear nothing will be.
#[cfg(feature = "server")]
pub(crate) async fn run_album_download(job: AlbumDownloadJob) -> AlbumDownloadOutcome {
    let AlbumDownloadJob {
        user_id,
        username: task_username,
        folder_path,
        query,
        require_flac,
        batch_id,
        backends,
        tx,
    } = job;

    if backends.is_empty() {
        let _ = tx.send(DownloadEvent::AutoDownload(AutoDownloadEvent::Failed {
            batch_id,
            error: NO_BACKENDS.to_string(),
        }));
        return AlbumDownloadOutcome::Failed(NO_BACKENDS.to_string());
    }

    // Build search description for logging and events
    let query_desc = query
        .album
        .as_ref()
        .map(|a| format!("{} - {}", &a.artist, &a.title))
        .unwrap_or_else(|| {
            query
                .tracks
                .first()
                .map(|t| format!("{} - {}", &t.artist, &t.title))
                .unwrap_or_else(|| "Unknown query".to_string())
        });
    let album = query.album;
    let tracks = query.tracks;

    // Send Searching event
    let _ = tx.send(DownloadEvent::AutoDownload(AutoDownloadEvent::Searching {
        batch_id: batch_id.clone(),
        query: query_desc.clone(),
        backend_count: backends.len(),
    }));

    info!(
        "Auto-download: searching {} backends for '{}'",
        backends.len(),
        query_desc
    );

    // Search all backends in parallel (per D-06)
    let search_futures: Vec<_> = backends
        .iter()
        .map(|(id, backend)| async {
            let groups = search_backend(id, backend, album.as_ref(), &tracks, require_flac).await;
            (id.clone(), groups)
        })
        .collect();

    let results = futures::future::join_all(search_futures).await;

    // Merge all results into a single list sorted by score (per D-07)
    let all_groups: Vec<DownloadableGroup> = results
        .into_iter()
        .flat_map(|(_backend_id, groups)| groups)
        .collect();

    let pick = pick_source(all_groups);
    let Some((result_count, best_score)) = pick.scoring() else {
        let _ = tx.send(DownloadEvent::AutoDownload(AutoDownloadEvent::Failed {
            batch_id: batch_id.clone(),
            error: "No results found".to_string(),
        }));
        info!("Auto-download: no results for '{}'", query_desc);
        return AlbumDownloadOutcome::NoResults;
    };

    // Send ScoringResults event
    let _ = tx.send(DownloadEvent::AutoDownload(
        AutoDownloadEvent::ScoringResults {
            batch_id: batch_id.clone(),
            result_count,
            best_score,
        },
    ));

    info!(
        "Auto-download: {} results, best score {:.2} for '{}'",
        result_count, best_score, query_desc
    );

    // Decide: auto-pick or fallback (per D-04)
    let (picked, all_groups) = match pick {
        SourcePick::Picked { picked, rest } => (picked, rest),
        SourcePick::BelowThreshold { groups, best_score } => {
            let _ = tx.send(DownloadEvent::AutoDownload(
                AutoDownloadEvent::FallbackToManual {
                    batch_id: batch_id.clone(),
                    results: groups,
                    best_score,
                    threshold: AUTO_SELECT_SCORE_THRESHOLD,
                },
            ));
            info!(
                "Auto-download: score {:.2} < threshold {:.1}, falling back to manual for '{}'",
                best_score, AUTO_SELECT_SCORE_THRESHOLD, query_desc
            );
            return AlbumDownloadOutcome::BelowThreshold {
                best_score: Some(best_score),
            };
        }
        SourcePick::NoResults => return AlbumDownloadOutcome::NoResults,
    };

    let _ = tx.send(DownloadEvent::AutoDownload(
        AutoDownloadEvent::PickedSource {
            batch_id: batch_id.clone(),
            source: picked.source.clone(),
            score: picked.score,
            quality: picked.quality.clone(),
            track_count: picked.items.len(),
        },
    ));

    info!(
        "Auto-download: picked source '{}' (score {:.2}, {} tracks) for '{}'",
        picked.source,
        picked.score,
        picked.items.len(),
        query_desc
    );

    let batch_label = super::auto_batch_label(&picked);

    // Create target directory
    let target_path_buf = std::path::Path::new(&folder_path).to_path_buf();
    if let Err(e) = tokio::fs::create_dir_all(&target_path_buf).await {
        let error = format!("Failed to create target directory: {}", e);
        let _ = tx.send(DownloadEvent::AutoDownload(AutoDownloadEvent::Failed {
            batch_id: batch_id.clone(),
            error: error.clone(),
        }));
        return AlbumDownloadOutcome::Failed(error);
    }

    // Queue download with the backend
    let items = picked.items.clone();
    let backend = match download_backend(None).await {
        Ok(b) => b,
        Err(e) => {
            let error = format!("Download backend not available: {}", e);
            let _ = tx.send(DownloadEvent::AutoDownload(AutoDownloadEvent::Failed {
                batch_id: batch_id.clone(),
                error: error.clone(),
            }));
            return AlbumDownloadOutcome::Failed(error);
        }
    };

    let fresh = BatchRef::new(
        TransferOwner {
            user_id,
            batch_id: batch_id.clone(),
        },
        batch_label,
        target_path_buf,
    );
    let enq = enqueue_request(&backend, items, fresh).await;
    let refusal = enq.refusal();
    let RequestEnqueue {
        batch,
        results,
        watcher,
        error,
        ..
    } = enq;
    let row = |p: DownloadProgress| p.with_batch(batch.owner.batch_id.clone(), batch.label.clone());

    // Send failed entries as progress events
    let failed_entries: Vec<DownloadProgress> = results
        .iter()
        .filter_map(|d| {
            let reason = failed_reason(&d.status)?;
            Some(row(DownloadProgress::failed(
                d.id.clone(),
                d.source.clone(),
                d.item.clone(),
                reason.to_string(),
            )))
        })
        .collect();
    if !failed_entries.is_empty() {
        let _ = tx.send(DownloadEvent::Progress(failed_entries));
    }

    let successful: Vec<QueuedDownload> = results
        .iter()
        .filter(|d| d.status == EnqueueStatus::Enqueued)
        .cloned()
        .collect();
    let already_downloading = results
        .iter()
        .any(|d| d.status == EnqueueStatus::AlreadyInFlight);

    let failure = enqueue_failure(
        error.as_ref().map(|e| e as &dyn std::fmt::Display),
        refusal,
        successful.len(),
        already_downloading,
    );
    let outcome = match &failure {
        Some(error) => {
            let _ = tx.send(DownloadEvent::AutoDownload(AutoDownloadEvent::Failed {
                batch_id: batch_id.clone(),
                error: error.clone(),
            }));
            AlbumDownloadOutcome::Failed(error.clone())
        }
        None => {
            // Send Downloading event
            let _ = tx.send(DownloadEvent::AutoDownload(
                AutoDownloadEvent::Downloading {
                    batch_id: batch_id.clone(),
                },
            ));
            AlbumDownloadOutcome::Enqueued {
                batch_id: batch_id.clone(),
                file_count: successful.len(),
            }
        }
    };

    // Send initial Queued progress for each track (with batch fields per D-10, D-11)
    let queued_entries: Vec<DownloadProgress> = successful
        .iter()
        .map(|d| {
            row(DownloadProgress::queued(
                d.id.clone(),
                d.source.clone(),
                d.item.clone(),
                d.size,
            ))
        })
        .collect();
    if !queued_entries.is_empty() {
        let _ = tx.send(DownloadEvent::Progress(queued_entries));
    }

    match watcher {
        Watcher::Joined(reservation) => {
            info!(
                "Auto-download: added {} tracks of '{}' to running batch {}",
                successful.len(),
                query_desc,
                batch.owner.batch_id
            );
            let lost = deliver(reservation, &successful, &backend, &batch).await;
            if !lost.is_empty() {
                let _ = tx.send(DownloadEvent::Progress(stopped_rows(&lost, &batch)));
            }
            return outcome;
        }
        Watcher::Own if successful.is_empty() && batch.inbox.close_if_idle() => return outcome,
        // Starts empty when nothing was enqueued but another request
        // joined the batch while this one was enqueueing.
        Watcher::Own => {}
    }

    let transfers: Vec<WatchedTransfer> = successful.iter().map(WatchedTransfer::from).collect();

    // Pool slots must line up with the monitor's slots, which follow
    // `successful`, not `picked.items`.
    let pool = SourcePool::from_tracks(&align_pool_tracks(&successful, &picked.items), &all_groups);

    info!(
        "Auto-download: queued {} tracks, starting monitor for '{}'",
        transfers.len(),
        query_desc
    );

    // Register task and run monitor (per D-05: normal DownloadProgress takes over)
    let task_cancellation = register_user_task(&task_username).await;

    tokio::spawn(async move {
        let mut monitor = DownloadMonitor::new(
            transfers,
            batch.target.clone(),
            tx,
            task_cancellation,
            task_username.clone(),
            batch,
        )
        .with_failover(pool.with_finder(Arc::new(BackendFinder { require_flac })));
        monitor.run().await;
        unregister_user_task(&task_username).await;
    });

    outcome
}

/// Auto-downloads a release for `user_id` with no request behind it, as the
/// auto-download endpoint would: into `folder_id`, which must be the
/// user's, else into the user's default download folder. `release_mbid` is
/// a MusicBrainz release id. Events go to the user's channel, which may
/// have no subscriber. An `Err` is a reason nothing was started.
#[cfg(feature = "server")]
pub(crate) async fn download_release_for_user(
    user_id: &str,
    release_mbid: &str,
    folder_id: Option<&str>,
) -> Result<AlbumDownloadOutcome, String> {
    if let Some(issue) = crate::importer_health::broken_issue().await {
        return Err(issue.title);
    }
    let user = crate::models::user::User::get_by_id(user_id).await?;
    let settings = UserSettings::get(user_id).await?;
    let folder_id = folder_id
        .map(str::to_string)
        .or(settings.default_download_folder_id)
        .ok_or_else(|| "No download folder given and no default download folder set".to_string())?;
    let folder = Folder::get_by_id(&folder_id)
        .await?
        .filter(|f| f.user_id == user_id)
        .ok_or_else(|| "Folder not found".to_string())?;

    // The search needs the release's title and artist, not just its id.
    let provider = crate::services::metadata_provider(None, None)
        .await
        .map_err(|e| format!("metadata provider unavailable: {e}"))?;
    let release = provider
        .get_album(release_mbid)
        .await
        .map_err(|e| format!("could not resolve release '{release_mbid}': {e}"))?;
    let mut album = release.album;
    album.id = release_mbid.to_string();
    let mut query = DownloadQuery {
        album: Some(album),
        tracks: release.tracks,
        backend: None,
    };
    crate::server_fns::search::hydrate_album_tracks(&mut query).await?;

    let (tx, _) = get_or_create_user_channel(&user.username).await;
    let job = AlbumDownloadJob {
        user_id: user.id,
        username: user.username,
        folder_path: folder.path,
        query,
        require_flac: settings.require_flac_only,
        batch_id: uuid::Uuid::new_v4().to_string(),
        backends: available_backends().await,
        tx,
    };
    Ok(run_album_download(job).await)
}

#[cfg(all(test, feature = "server"))]
mod tests {
    use super::*;
    use crate::server_fns::download::test_support::StubBackend;

    async fn research_flags(finder: BackendFinder) -> Vec<bool> {
        let stub = StubBackend::new(Vec::new());
        let backend: Arc<dyn DownloadBackend> = stub.clone();
        let found = finder.search(&backend, "Kowloon", "Two").await;
        assert!(found.expect("the stub's search answers").is_empty());
        stub.searches()
    }

    #[tokio::test(start_paused = true)]
    async fn a_flac_only_research_asks_the_backend_for_flac_only() {
        assert_eq!(
            research_flags(BackendFinder { require_flac: true }).await,
            vec![true]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_research_without_the_setting_takes_any_format() {
        assert_eq!(
            research_flags(BackendFinder {
                require_flac: false
            })
            .await,
            vec![false]
        );
    }

    use crate::server_fns::download::test_support::{group, item};

    fn scores(groups: &[DownloadableGroup]) -> Vec<f64> {
        groups.iter().map(|g| g.score).collect()
    }

    #[test]
    fn no_results_pick_nothing() {
        let pick = pick_source(Vec::new());
        assert!(matches!(pick, SourcePick::NoResults));
        assert_eq!(pick.scoring(), None);
    }

    #[test]
    fn a_best_score_below_the_threshold_picks_nothing() {
        let pick = pick_source(vec![
            group("a", vec![item("a", "One", "1.flac")], 0.3),
            group("b", vec![item("b", "One", "1.flac")], 0.6),
        ]);
        assert_eq!(pick.scoring(), Some((2, 0.6)));
        match pick {
            SourcePick::BelowThreshold { groups, best_score } => {
                assert_eq!(best_score, 0.6);
                assert_eq!(scores(&groups), vec![0.6, 0.3]);
            }
            other => panic!("expected BelowThreshold, got {other:?}"),
        }
    }

    #[test]
    fn the_best_score_at_the_threshold_is_picked_and_the_rest_kept_best_first() {
        let pick = pick_source(vec![
            group("a", vec![item("a", "One", "1.flac")], 0.4),
            group(
                "b",
                vec![item("b", "One", "1.flac")],
                AUTO_SELECT_SCORE_THRESHOLD,
            ),
            group("c", vec![item("c", "One", "1.flac")], 0.5),
        ]);
        assert_eq!(pick.scoring(), Some((3, AUTO_SELECT_SCORE_THRESHOLD)));
        match pick {
            SourcePick::Picked { picked, rest } => {
                assert_eq!(picked.source, "b");
                assert_eq!(scores(&rest), vec![0.5, 0.4]);
            }
            other => panic!("expected Picked, got {other:?}"),
        }
    }

    #[test]
    fn an_enqueue_fails_on_an_error_a_refusal_or_nothing_queued() {
        let error = "slskd down";
        assert_eq!(
            enqueue_failure(Some(&error), Some("refused"), 3, false),
            Some("Download queue failed: slskd down".to_string())
        );
        assert_eq!(
            enqueue_failure(None, Some("refused"), 3, false),
            Some("refused".to_string())
        );
        assert_eq!(
            enqueue_failure(None, None, 0, false),
            Some("All downloads failed to queue".to_string())
        );
        assert_eq!(enqueue_failure(None, None, 0, true), None);
        assert_eq!(enqueue_failure(None, None, 2, false), None);
    }

    fn job(
        backends: Vec<(String, Arc<dyn DownloadBackend>)>,
        tx: broadcast::Sender<DownloadEvent>,
    ) -> AlbumDownloadJob {
        AlbumDownloadJob {
            user_id: "user".to_string(),
            username: "background-user".to_string(),
            folder_path: "/music".to_string(),
            query: DownloadQuery::default(),
            require_flac: true,
            batch_id: "batch".to_string(),
            backends,
            tx,
        }
    }

    fn failed_error(event: DownloadEvent) -> String {
        match event {
            DownloadEvent::AutoDownload(AutoDownloadEvent::Failed { batch_id, error }) => {
                assert_eq!(batch_id, "batch");
                error
            }
            other => panic!("expected a Failed event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_download_without_backends_fails() {
        let (tx, mut rx) = broadcast::channel(16);
        let outcome = run_album_download(job(Vec::new(), tx)).await;
        assert_eq!(
            outcome,
            AlbumDownloadOutcome::Failed(NO_BACKENDS.to_string())
        );
        assert_eq!(failed_error(rx.try_recv().unwrap()), NO_BACKENDS);
    }

    #[tokio::test(start_paused = true)]
    async fn a_search_that_finds_nothing_reports_no_results() {
        let stub = StubBackend::new(Vec::new());
        let backend: Arc<dyn DownloadBackend> = stub.clone();
        let (tx, mut rx) = broadcast::channel(16);
        let outcome = run_album_download(job(vec![("stub".to_string(), backend)], tx)).await;

        assert_eq!(outcome, AlbumDownloadOutcome::NoResults);
        assert_eq!(stub.searches(), vec![true]);
        assert!(matches!(
            rx.try_recv().unwrap(),
            DownloadEvent::AutoDownload(AutoDownloadEvent::Searching {
                backend_count: 1,
                ..
            })
        ));
        assert_eq!(failed_error(rx.try_recv().unwrap()), "No results found");
    }

    #[tokio::test(start_paused = true)]
    async fn a_download_nobody_listens_to_still_runs() {
        let stub = StubBackend::new(Vec::new());
        let backend: Arc<dyn DownloadBackend> = stub.clone();
        // A background job: the user has no WebSocket open.
        let (tx, rx) = broadcast::channel(16);
        drop(rx);
        let outcome = run_album_download(job(vec![("stub".to_string(), backend)], tx)).await;

        assert_eq!(outcome, AlbumDownloadOutcome::NoResults);
        assert_eq!(stub.searches(), vec![true]);
    }
}
