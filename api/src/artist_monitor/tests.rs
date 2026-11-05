use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::models::followed_artist::tests::pool_with_user;

/// Release group id → release id; release id → download outcome.
struct FakeSources {
    groups: Mutex<Result<Vec<ReleaseGroupSummary>, String>>,
    releases: HashMap<String, Result<Option<String>, String>>,
    outcomes: HashMap<String, Result<AlbumDownloadOutcome, String>>,
    fetches: AtomicUsize,
    downloads: Mutex<Vec<(String, String, Option<String>)>>,
}

impl FakeSources {
    fn with_groups(groups: Vec<ReleaseGroupSummary>) -> Self {
        FakeSources {
            groups: Mutex::new(Ok(groups)),
            releases: HashMap::new(),
            outcomes: HashMap::new(),
            fetches: AtomicUsize::new(0),
            downloads: Mutex::new(Vec::new()),
        }
    }

    fn set_groups(&self, groups: Result<Vec<ReleaseGroupSummary>, String>) {
        *self.groups.lock().unwrap() = groups;
    }

    fn downloads(&self) -> Vec<(String, String, Option<String>)> {
        self.downloads.lock().unwrap().clone()
    }
}

#[async_trait]
impl MonitorSources for FakeSources {
    async fn release_groups(&self, _: &str) -> Result<Vec<ReleaseGroupSummary>, String> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        self.groups.lock().unwrap().clone()
    }

    async fn release_for_group(&self, rg: &str) -> Result<Option<String>, String> {
        self.releases
            .get(rg)
            .cloned()
            .unwrap_or_else(|| Ok(Some(format!("rel-{rg}"))))
    }

    async fn download(
        &self,
        user_id: &str,
        release_mbid: &str,
        folder_id: Option<&str>,
    ) -> Result<AlbumDownloadOutcome, String> {
        self.downloads.lock().unwrap().push((
            user_id.to_string(),
            release_mbid.to_string(),
            folder_id.map(str::to_string),
        ));
        self.outcomes
            .get(release_mbid)
            .cloned()
            .unwrap_or(Ok(AlbumDownloadOutcome::Enqueued {
                batch_id: "b".into(),
                file_count: 1,
            }))
    }
}

fn album(mbid: &str, date: &str) -> ReleaseGroupSummary {
    ReleaseGroupSummary {
        mbid: mbid.to_string(),
        title: format!("Album {mbid}"),
        primary_type: Some("Album".into()),
        secondary_types: vec![],
        first_release_date: Some(date.to_string()),
    }
}

async fn status_of(pool: &SqlitePool, user: &str, rg: &str) -> (ReleaseStatus, Option<String>) {
    let row = ArtistRelease::get(pool, user, rg).await.unwrap().unwrap();
    (row.status().unwrap(), row.status_detail)
}

#[tokio::test]
async fn a_baseline_check_stores_known_and_downloads_nothing() {
    let pool = pool_with_user("u1").await;
    FollowedArtist::follow(&pool, "u1", "base-a", "A")
        .await
        .unwrap();
    FollowedArtist::update_settings(&pool, "u1", "base-a", true, true, None)
        .await
        .unwrap();
    let mut live = album("live", "2001-01-01");
    live.secondary_types = vec!["Live".into()];
    let sources = FakeSources::with_groups(vec![
        album("rg1", "2001-01-01"),
        album("rg2", "2099-01-01"),
        live,
    ]);

    let summary = check_artist(&pool, &sources, "u1", "base-a", true)
        .await
        .unwrap();
    assert_eq!(
        summary,
        CheckSummary {
            added_known: 1,
            added_upcoming: 1,
            ..CheckSummary::default()
        }
    );
    assert_eq!(status_of(&pool, "u1", "rg1").await.0, ReleaseStatus::Known);
    assert_eq!(
        status_of(&pool, "u1", "rg2").await.0,
        ReleaseStatus::Upcoming
    );
    assert!(ArtistRelease::get(&pool, "u1", "live")
        .await
        .unwrap()
        .is_none());
    assert!(sources.downloads().is_empty());
    let follow = FollowedArtist::get(&pool, "u1", "base-a")
        .await
        .unwrap()
        .unwrap();
    assert!(follow.last_checked_at.is_some());
    assert!(follow.last_error.is_none());
}

#[tokio::test]
async fn a_later_check_records_an_extra_group_as_new() {
    let pool = pool_with_user("u1").await;
    FollowedArtist::follow(&pool, "u1", "later-a", "A")
        .await
        .unwrap();
    let sources = FakeSources::with_groups(vec![album("rg1", "2001-01-01")]);
    check_artist(&pool, &sources, "u1", "later-a", true)
        .await
        .unwrap();

    sources.set_groups(Ok(vec![
        album("rg1", "2001-01-01"),
        album("rg2", "2002-01-01"),
        album("rg3", "2099-01-01"),
    ]));
    let summary = check_artist(&pool, &sources, "u1", "later-a", false)
        .await
        .unwrap();
    assert_eq!(summary.added_new, 1);
    assert_eq!(summary.added_upcoming, 1);
    assert_eq!(summary.added_known, 0);
    assert_eq!(status_of(&pool, "u1", "rg1").await.0, ReleaseStatus::Known);
    assert_eq!(status_of(&pool, "u1", "rg2").await.0, ReleaseStatus::New);
    assert_eq!(
        status_of(&pool, "u1", "rg3").await.0,
        ReleaseStatus::Upcoming
    );
    // Auto download is off.
    assert!(sources.downloads().is_empty());

    // The upcoming one comes out.
    sources.set_groups(Ok(vec![album("rg3", "2001-06-01")]));
    let summary = check_artist(&pool, &sources, "u1", "later-a", false)
        .await
        .unwrap();
    assert_eq!(summary.became_new, 1);
    let row = ArtistRelease::get(&pool, "u1", "rg3")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status(), Some(ReleaseStatus::New));
    assert_eq!(row.first_release_date.as_deref(), Some("2001-06-01"));
    // rg1 and rg2 are no longer listed and stay as they were.
    assert_eq!(status_of(&pool, "u1", "rg2").await.0, ReleaseStatus::New);
}

#[tokio::test]
async fn auto_download_maps_each_outcome_to_a_status() {
    let pool = pool_with_user("u1").await;
    FollowedArtist::follow(&pool, "u1", "auto-a", "A")
        .await
        .unwrap();
    FollowedArtist::update_settings(&pool, "u1", "auto-a", false, true, Some("f1"))
        .await
        .unwrap();
    let mut sources = FakeSources::with_groups(vec![]);
    check_artist(&pool, &sources, "u1", "auto-a", true)
        .await
        .unwrap();

    sources.outcomes.insert(
        "rel-low".into(),
        Ok(AlbumDownloadOutcome::BelowThreshold {
            best_score: Some(0.5),
        }),
    );
    sources
        .outcomes
        .insert("rel-none".into(), Ok(AlbumDownloadOutcome::NoResults));
    sources.outcomes.insert(
        "rel-fail".into(),
        Ok(AlbumDownloadOutcome::Failed("slskd down".into())),
    );
    sources
        .outcomes
        .insert("rel-err".into(), Err("importer broken".into()));
    sources.releases.insert("norel".into(), Ok(None));
    sources
        .releases
        .insert("mberr".into(), Err("MusicBrainz: timeout".into()));
    // Oldest first, five per check: "mberr" is the newest and waits.
    sources.set_groups(Ok(vec![
        album("mberr", "2006-01-01"),
        album("ok", "2000-01-01"),
        album("low", "2001-01-01"),
        album("none", "2002-01-01"),
        album("fail", "2003-01-01"),
        album("err", "2004-01-01"),
        album("norel", "2005-01-01"),
    ]));

    // Seven new: five downloads tried, then a second check for the rest.
    let summary = check_artist(&pool, &sources, "u1", "auto-a", false)
        .await
        .unwrap();
    assert_eq!(summary.added_new, 7);
    assert_eq!((summary.queued, summary.manual, summary.failed), (1, 1, 3));
    let downloads = sources.downloads();
    let tried: Vec<&str> = downloads.iter().map(|d| d.1.as_str()).collect();
    assert_eq!(
        tried,
        vec!["rel-ok", "rel-low", "rel-none", "rel-fail", "rel-err"]
    );
    assert!(downloads
        .iter()
        .all(|d| d.0 == "u1" && d.2.as_deref() == Some("f1")));

    assert_eq!(
        status_of(&pool, "u1", "ok").await,
        (ReleaseStatus::Queued, None)
    );
    let (status, detail) = status_of(&pool, "u1", "low").await;
    assert_eq!(status, ReleaseStatus::Manual);
    assert!(detail.unwrap().contains("0.50"));
    assert_eq!(
        status_of(&pool, "u1", "none").await,
        (ReleaseStatus::Failed, Some(NO_SOULSEEK_MATCH.to_string()))
    );
    assert_eq!(
        status_of(&pool, "u1", "fail").await,
        (ReleaseStatus::Failed, Some("slskd down".to_string()))
    );
    assert_eq!(
        status_of(&pool, "u1", "err").await,
        (ReleaseStatus::Failed, Some("importer broken".to_string()))
    );
    assert_eq!(status_of(&pool, "u1", "norel").await.0, ReleaseStatus::New);
    assert_eq!(status_of(&pool, "u1", "mberr").await.0, ReleaseStatus::New);

    let summary = check_artist(&pool, &sources, "u1", "auto-a", false)
        .await
        .unwrap();
    assert_eq!(summary.added_new, 0);
    assert_eq!(summary.failed, 2);
    assert_eq!(
        status_of(&pool, "u1", "norel").await,
        (ReleaseStatus::Failed, Some(NO_RELEASE.to_string()))
    );
    assert_eq!(
        status_of(&pool, "u1", "mberr").await,
        (
            ReleaseStatus::Failed,
            Some("MusicBrainz: timeout".to_string())
        )
    );
    // Rows already moved on are not downloaded again.
    assert_eq!(sources.downloads().len(), 5);
}

#[tokio::test]
async fn an_on_demand_download_retries_a_failed_release() {
    let pool = pool_with_user("u1").await;
    FollowedArtist::follow(&pool, "u1", "demand-a", "A")
        .await
        .unwrap();
    FollowedArtist::update_settings(&pool, "u1", "demand-a", false, false, Some("f9"))
        .await
        .unwrap();
    let mut sources = FakeSources::with_groups(vec![album("rg1", "2001-01-01")]);
    check_artist(&pool, &sources, "u1", "demand-a", true)
        .await
        .unwrap();
    sources
        .outcomes
        .insert("rel-rg1".into(), Ok(AlbumDownloadOutcome::NoResults));

    let status = download_followed_release_with(&pool, &sources, "u1", "rg1")
        .await
        .unwrap();
    assert_eq!(status, ReleaseStatus::Failed);

    sources.outcomes.clear();
    let status = download_followed_release_with(&pool, &sources, "u1", "rg1")
        .await
        .unwrap();
    assert_eq!(status, ReleaseStatus::Queued);
    assert_eq!(
        status_of(&pool, "u1", "rg1").await,
        (ReleaseStatus::Queued, None)
    );
    assert!(sources
        .downloads()
        .iter()
        .all(|d| d.2.as_deref() == Some("f9")));

    assert!(
        download_followed_release_with(&pool, &sources, "u1", "nope")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_failed_fetch_sets_last_error_and_leaves_the_rows() {
    let pool = pool_with_user("u1").await;
    FollowedArtist::follow(&pool, "u1", "fail-a", "A")
        .await
        .unwrap();
    let sources = FakeSources::with_groups(vec![album("rg1", "2001-01-01")]);
    check_artist(&pool, &sources, "u1", "fail-a", true)
        .await
        .unwrap();
    let checked = FollowedArtist::get(&pool, "u1", "fail-a")
        .await
        .unwrap()
        .unwrap()
        .last_checked_at;

    sources.set_groups(Err("MusicBrainz: 503".into()));
    let err = check_artist(&pool, &sources, "u1", "fail-a", false)
        .await
        .unwrap_err();
    assert_eq!(err, "MusicBrainz: 503");
    let follow = FollowedArtist::get(&pool, "u1", "fail-a")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(follow.last_error.as_deref(), Some("MusicBrainz: 503"));
    assert!(follow.last_checked_at.is_some() && checked.is_some());
    let rows = ArtistRelease::releases_for_artist(&pool, "u1", "fail-a")
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status(), Some(ReleaseStatus::Known));

    // The next good check clears the error.
    sources.set_groups(Ok(vec![album("rg1", "2001-01-01")]));
    check_artist(&pool, &sources, "u1", "fail-a", false)
        .await
        .unwrap();
    let follow = FollowedArtist::get(&pool, "u1", "fail-a")
        .await
        .unwrap()
        .unwrap();
    assert!(follow.last_error.is_none());
}

#[tokio::test]
async fn a_check_of_a_follow_already_being_checked_returns_at_once() {
    let pool = pool_with_user("u1").await;
    FollowedArtist::follow(&pool, "u1", "guard-a", "A")
        .await
        .unwrap();
    let sources = FakeSources::with_groups(vec![album("rg1", "2001-01-01")]);

    let held = RunningGuard::acquire("u1", "guard-a").unwrap();
    assert!(RunningGuard::acquire("u1", "guard-a").is_none());
    let summary = check_artist(&pool, &sources, "u1", "guard-a", true)
        .await
        .unwrap();
    assert!(summary.already_running);
    assert_eq!(sources.fetches.load(Ordering::SeqCst), 0);
    assert!(ArtistRelease::get(&pool, "u1", "rg1")
        .await
        .unwrap()
        .is_none());

    drop(held);
    let summary = check_artist(&pool, &sources, "u1", "guard-a", true)
        .await
        .unwrap();
    assert!(!summary.already_running);
    assert_eq!(sources.fetches.load(Ordering::SeqCst), 1);
    // The guard is released after a check.
    assert!(RunningGuard::acquire("u1", "guard-a").is_some());
}

#[tokio::test]
async fn checking_an_artist_not_followed_is_an_error() {
    let pool = pool_with_user("u1").await;
    let sources = FakeSources::with_groups(vec![]);
    assert!(check_artist(&pool, &sources, "u1", "missing-a", true)
        .await
        .is_err());
    assert_eq!(sources.fetches.load(Ordering::SeqCst), 0);
}
