//! Artist monitoring: checks followed artists on MusicBrainz for new
//! release groups, records them, and downloads released ones for follows
//! with auto download on.

pub mod plan;

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};

use async_trait::async_trait;
use shared::metadata::ReleaseGroupSummary;
use sqlx::SqlitePool;
use tracing::{info, warn};

use crate::db::DB;
use crate::models::artist_release::{ArtistRelease, ReleaseStatus};
use crate::models::followed_artist::FollowedArtist;
use crate::server_fns::download::{download_release_for_user, AlbumDownloadOutcome};

pub use plan::{plan_check, ReleaseChange, StoredRelease};

/// At most this many releases are auto-downloaded per check of a follow;
/// the rest stay New for the next check.
pub const MAX_AUTO_DOWNLOADS_PER_CHECK: usize = 5;

/// Reason stored on a release no Soulseek source matched.
pub const NO_SOULSEEK_MATCH: &str = "No match on Soulseek";

/// Reason stored on a release group MusicBrainz has no release for.
pub const NO_RELEASE: &str = "MusicBrainz has no release for this release group";

/// Where a check gets release groups from and hands downloads to. Tests
/// swap in fakes; production uses MusicBrainz and the auto downloader.
#[async_trait]
pub(crate) trait MonitorSources: Send + Sync {
    /// The artist's release groups, any type.
    async fn release_groups(&self, artist_mbid: &str) -> Result<Vec<ReleaseGroupSummary>, String>;
    /// The release to download for a release group.
    async fn release_for_group(&self, release_group_mbid: &str) -> Result<Option<String>, String>;
    /// Starts downloading `release_mbid` for the user.
    async fn download(
        &self,
        user_id: &str,
        release_mbid: &str,
        folder_id: Option<&str>,
    ) -> Result<AlbumDownloadOutcome, String>;
}

/// MusicBrainz and the auto downloader.
struct LiveSources;

#[async_trait]
impl MonitorSources for LiveSources {
    async fn release_groups(&self, artist_mbid: &str) -> Result<Vec<ReleaseGroupSummary>, String> {
        soulbeet::musicbrainz::artist_release_groups(artist_mbid)
            .await
            .map_err(|e| format!("MusicBrainz: {e}"))
    }

    async fn release_for_group(&self, release_group_mbid: &str) -> Result<Option<String>, String> {
        soulbeet::musicbrainz::release_group_release(release_group_mbid)
            .await
            .map_err(|e| format!("MusicBrainz: {e}"))
    }

    async fn download(
        &self,
        user_id: &str,
        release_mbid: &str,
        folder_id: Option<&str>,
    ) -> Result<AlbumDownloadOutcome, String> {
        download_release_for_user(user_id, release_mbid, folder_id).await
    }
}

/// What one check of a follow did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckSummary {
    /// Another check of the same follow was running, so this one did
    /// nothing.
    pub already_running: bool,
    /// Release groups recorded as New.
    pub added_new: usize,
    /// Release groups recorded as Upcoming.
    pub added_upcoming: usize,
    /// Release groups recorded as Known.
    pub added_known: usize,
    /// Upcoming releases that came out and became New.
    pub became_new: usize,
    /// Auto downloads enqueued.
    pub queued: usize,
    /// Auto downloads left for the user to pick a source.
    pub manual: usize,
    /// Auto downloads that could not start.
    pub failed: usize,
}

/// Follows being checked right now, as (user id, artist mbid).
static RUNNING: LazyLock<Mutex<HashSet<(String, String)>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// Marks a follow as being checked until dropped.
struct RunningGuard {
    key: (String, String),
}

impl RunningGuard {
    /// `None` while another check of the follow holds it.
    fn acquire(user_id: &str, artist_mbid: &str) -> Option<RunningGuard> {
        let key = (user_id.to_string(), artist_mbid.to_string());
        let mut running = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
        running.insert(key.clone()).then_some(RunningGuard { key })
    }
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        let mut running = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
        running.remove(&self.key);
    }
}

fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// Checks a follow now, as a baseline when it was never checked.
pub async fn check_artist_now(user_id: &str, artist_mbid: &str) -> Result<CheckSummary, String> {
    let follow = FollowedArtist::get(&DB, user_id, artist_mbid)
        .await?
        .ok_or_else(|| "Not following this artist".to_string())?;
    let baseline = follow.last_checked_at.is_none();
    check_artist(&DB, &LiveSources, user_id, artist_mbid, baseline).await
}

/// Downloads a followed artist's release group on demand and records the
/// outcome on it. Any status is accepted.
pub async fn download_followed_release(
    user_id: &str,
    release_group_mbid: &str,
) -> Result<ReleaseStatus, String> {
    download_followed_release_with(&DB, &LiveSources, user_id, release_group_mbid).await
}

/// Checks every follow, of every user, never checked or last checked at
/// least `older_than_hours` ago, one after another (the MusicBrainz rate
/// limit is process-wide).
pub async fn run_due_checks(older_than_hours: i64) {
    let due = match FollowedArtist::due_for_check(&DB, older_than_hours).await {
        Ok(due) => due,
        Err(e) => {
            warn!("Artist monitoring: could not list follows due for a check: {e}");
            return;
        }
    };
    if due.is_empty() {
        return;
    }
    let mut total = CheckSummary::default();
    let mut errors = 0usize;
    for follow in &due {
        match check_artist_now(&follow.user_id, &follow.artist_mbid).await {
            Ok(s) => {
                total.added_new += s.added_new;
                total.added_upcoming += s.added_upcoming;
                total.added_known += s.added_known;
                total.became_new += s.became_new;
                total.queued += s.queued;
                total.manual += s.manual;
                total.failed += s.failed;
            }
            Err(e) => {
                errors += 1;
                warn!(
                    "Artist monitoring: check of {} ({}) for user {} failed: {e}",
                    follow.name, follow.artist_mbid, follow.user_id
                );
            }
        }
    }
    info!(
        "Artist monitoring: checked {} follows ({} failed): {} new, {} upcoming, {} known, \
         {} released; downloads {} queued, {} manual, {} failed",
        due.len(),
        errors,
        total.added_new,
        total.added_upcoming,
        total.added_known,
        total.became_new,
        total.queued,
        total.manual,
        total.failed
    );
}

/// Checks one follow: records the artist's new release groups and moves
/// released Upcoming ones to New, then, with auto download on, downloads
/// up to [`MAX_AUTO_DOWNLOADS_PER_CHECK`] New ones, oldest first. On a
/// `baseline` check everything unseen is Known. A check of a follow
/// already being checked returns at once with `already_running`.
pub(crate) async fn check_artist(
    pool: &SqlitePool,
    sources: &dyn MonitorSources,
    user_id: &str,
    artist_mbid: &str,
    baseline: bool,
) -> Result<CheckSummary, String> {
    let Some(_guard) = RunningGuard::acquire(user_id, artist_mbid) else {
        return Ok(CheckSummary {
            already_running: true,
            ..CheckSummary::default()
        });
    };
    let follow = FollowedArtist::get(pool, user_id, artist_mbid)
        .await?
        .ok_or_else(|| "Not following this artist".to_string())?;

    let fetched = match sources.release_groups(artist_mbid).await {
        Ok(fetched) => fetched,
        Err(e) => {
            FollowedArtist::record_check_failure(pool, user_id, artist_mbid, &e).await?;
            return Err(e);
        }
    };

    // Keyed across all the user's follows, so a group credited to two
    // followed artists is recorded once.
    let stored: HashMap<String, StoredRelease> = ArtistRelease::all_for_user(pool, user_id)
        .await?
        .into_iter()
        .filter_map(|row| {
            let status = row.status()?;
            Some((
                row.release_group_mbid,
                StoredRelease {
                    status,
                    first_release_date: row.first_release_date,
                },
            ))
        })
        .collect();
    let changes = plan_check(
        &stored,
        &fetched,
        follow.include_singles,
        baseline,
        &today(),
    );

    let mut summary = CheckSummary::default();
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    for change in &changes {
        match change {
            ReleaseChange::Insert {
                release_group_mbid,
                title,
                kind,
                first_release_date,
                status,
            } => {
                ArtistRelease::insert(
                    &mut *tx,
                    user_id,
                    artist_mbid,
                    release_group_mbid,
                    title,
                    *kind,
                    first_release_date.as_deref(),
                    *status,
                )
                .await?;
                match status {
                    ReleaseStatus::New => summary.added_new += 1,
                    ReleaseStatus::Upcoming => summary.added_upcoming += 1,
                    _ => summary.added_known += 1,
                }
            }
            ReleaseChange::Transition {
                release_group_mbid,
                to,
                first_release_date,
            } => {
                ArtistRelease::transition(
                    &mut *tx,
                    user_id,
                    release_group_mbid,
                    *to,
                    first_release_date.as_deref(),
                )
                .await?;
                summary.became_new += 1;
            }
        }
    }
    FollowedArtist::record_check_success(&mut *tx, user_id, artist_mbid).await?;
    tx.commit().await.map_err(|e| e.to_string())?;

    if follow.auto_download {
        let mut fresh: Vec<ArtistRelease> =
            ArtistRelease::releases_for_artist(pool, user_id, artist_mbid)
                .await?
                .into_iter()
                .filter(|r| r.status() == Some(ReleaseStatus::New))
                .collect();
        // Oldest first; undated ones last.
        fresh.sort_by(|a, b| {
            (
                a.first_release_date.is_none(),
                &a.first_release_date,
                &a.discovered_at,
            )
                .cmp(&(
                    b.first_release_date.is_none(),
                    &b.first_release_date,
                    &b.discovered_at,
                ))
        });
        for release in fresh.iter().take(MAX_AUTO_DOWNLOADS_PER_CHECK) {
            let (status, detail) = try_download(
                sources,
                user_id,
                &release.release_group_mbid,
                follow.folder_id.as_deref(),
            )
            .await;
            ArtistRelease::set_status(
                pool,
                user_id,
                &release.release_group_mbid,
                status,
                detail.as_deref(),
            )
            .await?;
            match status {
                ReleaseStatus::Queued => summary.queued += 1,
                ReleaseStatus::Manual => summary.manual += 1,
                _ => summary.failed += 1,
            }
        }
    }
    Ok(summary)
}

/// [`download_followed_release`] against `pool` and `sources`.
pub(crate) async fn download_followed_release_with(
    pool: &SqlitePool,
    sources: &dyn MonitorSources,
    user_id: &str,
    release_group_mbid: &str,
) -> Result<ReleaseStatus, String> {
    let release = ArtistRelease::get(pool, user_id, release_group_mbid)
        .await?
        .ok_or_else(|| "Release not found".to_string())?;
    let folder_id = FollowedArtist::get(pool, user_id, &release.artist_mbid)
        .await?
        .and_then(|f| f.folder_id);
    let (status, detail) =
        try_download(sources, user_id, release_group_mbid, folder_id.as_deref()).await;
    ArtistRelease::set_status(pool, user_id, release_group_mbid, status, detail.as_deref()).await?;
    Ok(status)
}

/// Downloads a release group and says what its status becomes: Queued,
/// Manual when the best match was too weak, else Failed with the reason.
async fn try_download(
    sources: &dyn MonitorSources,
    user_id: &str,
    release_group_mbid: &str,
    folder_id: Option<&str>,
) -> (ReleaseStatus, Option<String>) {
    let release = match sources.release_for_group(release_group_mbid).await {
        Ok(Some(release)) => release,
        Ok(None) => return (ReleaseStatus::Failed, Some(NO_RELEASE.to_string())),
        Err(e) => return (ReleaseStatus::Failed, Some(e)),
    };
    match sources.download(user_id, &release, folder_id).await {
        Ok(AlbumDownloadOutcome::Enqueued { .. }) => (ReleaseStatus::Queued, None),
        Ok(AlbumDownloadOutcome::BelowThreshold { best_score }) => (
            ReleaseStatus::Manual,
            best_score.map(|s| format!("Best Soulseek match scored {s:.2}")),
        ),
        Ok(AlbumDownloadOutcome::NoResults) => {
            (ReleaseStatus::Failed, Some(NO_SOULSEEK_MATCH.to_string()))
        }
        Ok(AlbumDownloadOutcome::Failed(reason)) | Err(reason) => {
            (ReleaseStatus::Failed, Some(reason))
        }
    }
}

#[cfg(test)]
mod tests;
