//! Server functions for artist monitoring: searching MusicBrainz artists,
//! following them, and acting on the releases the background check finds.

use dioxus::prelude::*;

pub use shared::artists::{
    ArtistPage, ArtistReleaseView, CheckResult, FollowSettings, FollowedArtistView, NewReleaseView,
    ReleaseDownloadResult,
};
pub use shared::metadata::ArtistSummary;

#[cfg(feature = "server")]
use super::{refusal_error, server_error};
#[cfg(feature = "server")]
use crate::AuthSession;

#[cfg(feature = "server")]
mod logic {
    use std::collections::{HashMap, HashSet};
    use std::sync::{LazyLock, Mutex};
    use std::time::{Duration, Instant};

    use dioxus::logger::tracing::debug;
    use shared::artists::{ArtistReleaseView, FollowSettings, FollowedArtistView, NewReleaseView};
    use shared::metadata::{ArtistSummary, ReleaseGroupSummary, ReleaseKind};

    use crate::models::artist_release::{ArtistRelease, ReleaseStatus};
    use crate::models::followed_artist::FollowedArtist;

    /// How long an artist's MusicBrainz data is reused.
    pub const CACHE_TTL: Duration = Duration::from_secs(10 * 60);

    pub type MbData = (ArtistSummary, Vec<ReleaseGroupSummary>);

    /// Whether data stored at `stored_at` can still be used at `now`.
    pub fn is_fresh(stored_at: Instant, now: Instant, ttl: Duration) -> bool {
        now.saturating_duration_since(stored_at) < ttl
    }

    static CACHE: LazyLock<Mutex<HashMap<String, (Instant, MbData)>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    pub fn cache_get(mbid: &str) -> Option<MbData> {
        let mut cache = CACHE.lock().ok()?;
        let now = Instant::now();
        match cache.get(mbid) {
            Some((at, data)) if is_fresh(*at, now, CACHE_TTL) => Some(data.clone()),
            Some(_) => {
                cache.remove(mbid);
                None
            }
            None => None,
        }
    }

    pub fn cache_put(mbid: &str, data: MbData) {
        if let Ok(mut cache) = CACHE.lock() {
            let now = Instant::now();
            cache.retain(|_, (at, _)| is_fresh(*at, now, CACHE_TTL));
            cache.insert(mbid.to_string(), (now, data));
        }
    }

    pub fn cache_invalidate(mbid: &str) {
        if let Ok(mut cache) = CACHE.lock() {
            cache.remove(mbid);
        }
    }

    /// Whether a release of `kind` is listed. Singles show when the user
    /// follows with singles on, or does not follow at all.
    pub fn visible(kind: ReleaseKind, include_singles: Option<bool>) -> bool {
        kind != ReleaseKind::Single || include_singles.unwrap_or(true)
    }

    /// Lowercases, drops bracketed parts such as "(Deluxe Edition)" or
    /// "[Remastered]", turns punctuation into spaces and collapses spaces.
    pub fn normalize_title(title: &str) -> String {
        let mut kept = String::with_capacity(title.len());
        let mut depth = 0usize;
        for c in title.chars() {
            match c {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth = depth.saturating_sub(1),
                _ if depth == 0 => kept.push(c),
                _ => {}
            }
        }
        kept.to_lowercase()
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { ' ' })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The (artist, title) pairs of the user's Navidrome albums, normalized.
    /// Empty when Navidrome is not connected or fails.
    pub async fn library_albums(user_id: &str) -> HashSet<(String, String)> {
        let Ok(client) = crate::services::navidrome_client_for_user(user_id).await else {
            return HashSet::new();
        };
        match client.get_all_albums().await {
            Ok(albums) => albums
                .iter()
                .map(|a| {
                    (
                        normalize_title(a.artist.as_deref().unwrap_or("")),
                        normalize_title(a.display_name()),
                    )
                })
                .collect(),
            Err(e) => {
                debug!("Navidrome album lookup for in_library failed: {e}");
                HashSet::new()
            }
        }
    }

    /// Whether the library holds `title` by `artist_name`.
    pub fn in_library(
        library: &HashSet<(String, String)>,
        artist_name: &str,
        title: &str,
        status: Option<&str>,
    ) -> bool {
        if status == Some(ReleaseStatus::Queued.as_str()) || library.is_empty() {
            return false;
        }
        let artist = normalize_title(artist_name);
        let title = normalize_title(title);
        if title.is_empty() {
            return false;
        }
        library.iter().any(|(a, t)| {
            *t == title
                && (*a == artist
                    || (!a.is_empty()
                        && !artist.is_empty()
                        && (a.contains(&artist) || artist.contains(a.as_str()))))
        })
    }

    pub fn settings_of(f: &FollowedArtist) -> FollowSettings {
        FollowSettings {
            include_singles: f.include_singles,
            auto_download: f.auto_download,
            folder_id: f.folder_id.clone(),
        }
    }

    pub fn release_view(row: &ArtistRelease, in_library: bool) -> Option<ArtistReleaseView> {
        Some(ArtistReleaseView {
            release_group_mbid: row.release_group_mbid.clone(),
            title: row.title.clone(),
            kind: row.kind()?,
            first_release_date: row.first_release_date.clone(),
            status: Some(row.status.clone()),
            status_detail: row.status_detail.clone(),
            in_library,
        })
    }

    pub fn followed_view(f: &FollowedArtist, rows: &[ArtistRelease]) -> FollowedArtistView {
        let count = |s: ReleaseStatus| {
            rows.iter()
                .filter(|r| r.artist_mbid == f.artist_mbid && r.status == s.as_str())
                .count() as u32
        };
        FollowedArtistView {
            artist_mbid: f.artist_mbid.clone(),
            name: f.name.clone(),
            settings: settings_of(f),
            last_checked_at: f.last_checked_at.clone(),
            last_error: f.last_error.clone(),
            new_count: count(ReleaseStatus::New),
            upcoming_count: count(ReleaseStatus::Upcoming),
        }
    }

    /// Orders case-insensitively by name.
    pub fn sort_followed(list: &mut [FollowedArtistView]) {
        list.sort_by_cached_key(|v| (v.name.to_lowercase(), v.artist_mbid.clone()));
    }

    fn status_rank(status: Option<&str>) -> u8 {
        match status.and_then(ReleaseStatus::parse) {
            Some(ReleaseStatus::New) => 0,
            Some(ReleaseStatus::Manual) => 1,
            Some(ReleaseStatus::Failed) => 2,
            Some(ReleaseStatus::Upcoming) => 3,
            Some(ReleaseStatus::Queued) => 4,
            _ => 5,
        }
    }

    /// New, Manual, Failed, Upcoming, Queued; within a status the newest
    /// date first, undated last.
    pub fn sort_new_releases(list: &mut [NewReleaseView]) {
        list.sort_by(|a, b| {
            let (ra, rb) = (&a.release, &b.release);
            status_rank(ra.status.as_deref())
                .cmp(&status_rank(rb.status.as_deref()))
                .then_with(|| match (&ra.first_release_date, &rb.first_release_date) {
                    (Some(x), Some(y)) => y.cmp(x),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                })
                .then_with(|| ra.title.cmp(&rb.title))
        });
    }

    /// Merges MusicBrainz release groups with the stored rows.
    pub fn merge_releases(
        groups: &[ReleaseGroupSummary],
        rows: &[ArtistRelease],
        include_singles: Option<bool>,
        artist_name: &str,
        library: &HashSet<(String, String)>,
    ) -> Vec<ArtistReleaseView> {
        let stored: HashMap<&str, &ArtistRelease> = rows
            .iter()
            .map(|r| (r.release_group_mbid.as_str(), r))
            .collect();
        groups
            .iter()
            .filter_map(|g| {
                let kind = g.kind()?;
                if !visible(kind, include_singles) {
                    return None;
                }
                let row = stored.get(g.mbid.as_str());
                let status = row.map(|r| r.status.clone());
                Some(ArtistReleaseView {
                    release_group_mbid: g.mbid.clone(),
                    title: g.title.clone(),
                    kind,
                    first_release_date: g
                        .first_release_date
                        .clone()
                        .or_else(|| row.and_then(|r| r.first_release_date.clone())),
                    status_detail: row.and_then(|r| r.status_detail.clone()),
                    in_library: in_library(library, artist_name, &g.title, status.as_deref()),
                    status,
                })
            })
            .collect()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn view(status: &str, date: Option<&str>, title: &str) -> NewReleaseView {
            NewReleaseView {
                artist_mbid: "a".into(),
                artist_name: "A".into(),
                release: ArtistReleaseView {
                    release_group_mbid: title.into(),
                    title: title.into(),
                    kind: ReleaseKind::Album,
                    first_release_date: date.map(String::from),
                    status: Some(status.into()),
                    status_detail: None,
                    in_library: false,
                },
            }
        }

        #[test]
        fn normalizes_titles() {
            assert_eq!(normalize_title("Abbey Road (Deluxe Edition)"), "abbey road");
            assert_eq!(normalize_title("OK Computer [Remastered]"), "ok computer");
            assert_eq!(normalize_title("  Whose   Next? "), "whose next");
            assert_eq!(normalize_title("Sigur Ros - ()"), "sigur ros");
            assert_eq!(normalize_title("(Untitled)"), "");
        }

        #[test]
        fn library_match_ignores_decoration_and_queued() {
            let lib: HashSet<(String, String)> = [(
                normalize_title("Radiohead"),
                normalize_title("OK Computer (Remastered)"),
            )]
            .into();
            assert!(in_library(&lib, "Radiohead", "OK Computer", Some("known")));
            assert!(in_library(&lib, "Radiohead", "OK Computer", None));
            assert!(!in_library(
                &lib,
                "Radiohead",
                "OK Computer",
                Some("queued")
            ));
            assert!(!in_library(&lib, "Muse", "OK Computer", None));
            assert!(!in_library(&lib, "Radiohead", "Kid A", None));
            assert!(!in_library(
                &HashSet::new(),
                "Radiohead",
                "OK Computer",
                None
            ));
        }

        #[test]
        fn new_releases_sort_by_status_then_newest() {
            let mut list = vec![
                view("queued", Some("2024-01-01"), "q"),
                view("upcoming", Some("2030-01-01"), "u"),
                view("failed", Some("2024-01-01"), "f"),
                view("new", Some("2023-01-01"), "n-old"),
                view("manual", Some("2024-01-01"), "m"),
                view("new", Some("2024-06-01"), "n-new"),
                view("new", None, "n-undated"),
            ];
            sort_new_releases(&mut list);
            let order: Vec<&str> = list.iter().map(|v| v.release.title.as_str()).collect();
            assert_eq!(order, ["n-new", "n-old", "n-undated", "m", "f", "u", "q"]);
        }

        #[test]
        fn singles_visibility() {
            assert!(visible(ReleaseKind::Album, Some(false)));
            assert!(visible(ReleaseKind::Ep, Some(false)));
            assert!(!visible(ReleaseKind::Single, Some(false)));
            assert!(visible(ReleaseKind::Single, Some(true)));
            assert!(visible(ReleaseKind::Single, None));
        }

        #[test]
        fn cache_expires_after_ttl() {
            let t0 = Instant::now();
            let ttl = Duration::from_secs(600);
            assert!(is_fresh(t0, t0, ttl));
            assert!(is_fresh(t0, t0 + Duration::from_secs(599), ttl));
            assert!(!is_fresh(t0, t0 + ttl, ttl));
            assert!(is_fresh(t0 + Duration::from_secs(5), t0, ttl));
        }

        #[test]
        fn followed_sort_ignores_case() {
            let mk = |name: &str| FollowedArtistView {
                artist_mbid: name.into(),
                name: name.into(),
                settings: FollowSettings {
                    include_singles: false,
                    auto_download: false,
                    folder_id: None,
                },
                last_checked_at: None,
                last_error: None,
                new_count: 0,
                upcoming_count: 0,
            };
            let mut v = vec![mk("beta"), mk("Alpha"), mk("Charlie")];
            sort_followed(&mut v);
            let names: Vec<&str> = v.iter().map(|x| x.name.as_str()).collect();
            assert_eq!(names, ["Alpha", "beta", "Charlie"]);
        }
    }
}

/// Search MusicBrainz for artists by name.
#[get("/api/artists/search", auth: AuthSession)]
pub async fn search_artists(query: String) -> Result<Vec<ArtistSummary>, ServerFnError> {
    #[cfg(feature = "server")]
    {
        let _ = &auth;
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        soulbeet::musicbrainz::search_artists(query, 10)
            .await
            .map_err(server_error)
    }
    #[cfg(not(feature = "server"))]
    {
        let _ = query;
        unreachable!()
    }
}

/// An artist with their releases, merged with what the user's follow tracks.
#[get("/api/artists/page", auth: AuthSession)]
pub async fn get_artist_page(mbid: String) -> Result<ArtistPage, ServerFnError> {
    #[cfg(feature = "server")]
    {
        use crate::db::DB;
        use crate::models::artist_release::ArtistRelease;
        use crate::models::followed_artist::FollowedArtist;

        let user_id = &auth.0.sub;
        let (artist, groups) = match logic::cache_get(&mbid) {
            Some(data) => data,
            None => {
                let artist = soulbeet::musicbrainz::get_artist(&mbid)
                    .await
                    .map_err(server_error)?;
                let groups = soulbeet::musicbrainz::artist_release_groups(&mbid)
                    .await
                    .map_err(server_error)?;
                let groups: Vec<_> = groups.into_iter().filter(|g| g.kind().is_some()).collect();
                logic::cache_put(&mbid, (artist.clone(), groups.clone()));
                (artist, groups)
            }
        };
        let follow = FollowedArtist::get(&DB, user_id, &mbid)
            .await
            .map_err(server_error)?;
        let rows = ArtistRelease::releases_for_artist(&DB, user_id, &mbid)
            .await
            .map_err(server_error)?;
        let library = logic::library_albums(user_id).await;
        let releases = logic::merge_releases(
            &groups,
            &rows,
            follow.as_ref().map(|f| f.include_singles),
            &artist.name,
            &library,
        );
        Ok(ArtistPage {
            artist,
            last_checked_at: follow.as_ref().and_then(|f| f.last_checked_at.clone()),
            last_error: follow.as_ref().and_then(|f| f.last_error.clone()),
            follow: follow.as_ref().map(logic::settings_of),
            releases,
        })
    }
    #[cfg(not(feature = "server"))]
    {
        let _ = mbid;
        unreachable!()
    }
}

/// Follow an artist; the baseline check runs in the background.
#[post("/api/artists/follow", auth: AuthSession)]
pub async fn follow_artist(
    mbid: String,
    name: String,
) -> Result<FollowedArtistView, ServerFnError> {
    #[cfg(feature = "server")]
    {
        use crate::db::DB;
        use crate::models::followed_artist::FollowedArtist;

        let user_id = auth.0.sub.clone();
        let follow = FollowedArtist::follow(&DB, &user_id, &mbid, name.trim())
            .await
            .map_err(server_error)?;
        let check_mbid = mbid.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::artist_monitor::check_artist_now(&user_id, &check_mbid).await {
                dioxus::logger::tracing::warn!("Baseline check of {check_mbid} failed: {e}");
            }
        });
        Ok(logic::followed_view(&follow, &[]))
    }
    #[cfg(not(feature = "server"))]
    {
        let _ = (mbid, name);
        unreachable!()
    }
}

/// Stop following an artist, dropping its tracked releases.
#[post("/api/artists/unfollow", auth: AuthSession)]
pub async fn unfollow_artist(mbid: String) -> Result<(), ServerFnError> {
    #[cfg(feature = "server")]
    {
        use crate::db::DB;
        use crate::models::followed_artist::FollowedArtist;

        FollowedArtist::unfollow(&DB, &auth.0.sub, &mbid)
            .await
            .map_err(server_error)?;
        Ok(())
    }
    #[cfg(not(feature = "server"))]
    {
        let _ = mbid;
        unreachable!()
    }
}

/// Change how a followed artist is monitored.
#[post("/api/artists/settings", auth: AuthSession)]
pub async fn update_follow(mbid: String, settings: FollowSettings) -> Result<(), ServerFnError> {
    #[cfg(feature = "server")]
    {
        use crate::db::DB;
        use crate::models::folder::Folder;
        use crate::models::followed_artist::FollowedArtist;

        let user_id = &auth.0.sub;
        if let Some(folder_id) = &settings.folder_id {
            match Folder::get_by_id(folder_id).await {
                Ok(Some(f)) if f.user_id == *user_id => {}
                Ok(_) => return Err(refusal_error("Folder not found")),
                Err(e) => return Err(server_error(e)),
            }
        }
        let updated = FollowedArtist::update_settings(
            &DB,
            user_id,
            &mbid,
            settings.include_singles,
            settings.auto_download,
            settings.folder_id.as_deref(),
        )
        .await
        .map_err(server_error)?;
        if !updated {
            return Err(refusal_error("Not following this artist"));
        }
        Ok(())
    }
    #[cfg(not(feature = "server"))]
    {
        let _ = (mbid, settings);
        unreachable!()
    }
}

/// The artists the user follows, with counts of releases needing attention.
#[get("/api/artists/followed", auth: AuthSession)]
pub async fn list_followed_artists() -> Result<Vec<FollowedArtistView>, ServerFnError> {
    #[cfg(feature = "server")]
    {
        use crate::db::DB;
        use crate::models::artist_release::ArtistRelease;
        use crate::models::followed_artist::FollowedArtist;

        let user_id = &auth.0.sub;
        let follows = FollowedArtist::list_for_user(&DB, user_id)
            .await
            .map_err(server_error)?;
        let rows = ArtistRelease::all_for_user(&DB, user_id)
            .await
            .map_err(server_error)?;
        let mut views: Vec<_> = follows
            .iter()
            .map(|f| logic::followed_view(f, &rows))
            .collect();
        logic::sort_followed(&mut views);
        Ok(views)
    }
    #[cfg(not(feature = "server"))]
    {
        unreachable!()
    }
}

/// Releases of followed artists that are new, upcoming or in progress.
#[get("/api/artists/new-releases", auth: AuthSession)]
pub async fn list_new_releases() -> Result<Vec<NewReleaseView>, ServerFnError> {
    #[cfg(feature = "server")]
    {
        use std::collections::HashMap;

        use crate::db::DB;
        use crate::models::artist_release::{ArtistRelease, ReleaseStatus};
        use crate::models::followed_artist::FollowedArtist;

        let user_id = &auth.0.sub;
        let rows = ArtistRelease::releases_by_status(
            &DB,
            user_id,
            &[
                ReleaseStatus::New,
                ReleaseStatus::Manual,
                ReleaseStatus::Failed,
                ReleaseStatus::Queued,
                ReleaseStatus::Upcoming,
            ],
        )
        .await
        .map_err(server_error)?;
        let follows = FollowedArtist::list_for_user(&DB, user_id)
            .await
            .map_err(server_error)?;
        let names: HashMap<&str, &str> = follows
            .iter()
            .map(|f| (f.artist_mbid.as_str(), f.name.as_str()))
            .collect();
        let mut views: Vec<NewReleaseView> = rows
            .iter()
            .filter_map(|row| {
                let artist_name = names.get(row.artist_mbid.as_str())?;
                Some(NewReleaseView {
                    artist_mbid: row.artist_mbid.clone(),
                    artist_name: artist_name.to_string(),
                    release: logic::release_view(row, false)?,
                })
            })
            .collect();
        logic::sort_new_releases(&mut views);
        Ok(views)
    }
    #[cfg(not(feature = "server"))]
    {
        unreachable!()
    }
}

/// Hide a release from the new releases list.
#[post("/api/artists/dismiss", auth: AuthSession)]
pub async fn dismiss_release(rg_mbid: String) -> Result<(), ServerFnError> {
    #[cfg(feature = "server")]
    {
        use crate::db::DB;
        use crate::models::artist_release::{ArtistRelease, ReleaseStatus};

        let found =
            ArtistRelease::set_status(&*DB, &auth.0.sub, &rg_mbid, ReleaseStatus::Dismissed, None)
                .await
                .map_err(server_error)?;
        if !found {
            return Err(refusal_error("Release not found"));
        }
        Ok(())
    }
    #[cfg(not(feature = "server"))]
    {
        let _ = rg_mbid;
        unreachable!()
    }
}

/// Download a tracked release of a followed artist now.
#[post("/api/artists/download", auth: AuthSession)]
pub async fn download_release(rg_mbid: String) -> Result<ReleaseDownloadResult, ServerFnError> {
    #[cfg(feature = "server")]
    {
        use crate::db::DB;
        use crate::models::artist_release::ArtistRelease;

        let user_id = &auth.0.sub;
        if ArtistRelease::get(&DB, user_id, &rg_mbid)
            .await
            .map_err(server_error)?
            .is_none()
        {
            return Err(refusal_error(
                "This release is not tracked; follow the artist first",
            ));
        }
        let status = crate::artist_monitor::download_followed_release(user_id, &rg_mbid)
            .await
            .map_err(server_error)?;
        let detail = ArtistRelease::get(&DB, user_id, &rg_mbid)
            .await
            .map_err(server_error)?
            .and_then(|r| r.status_detail);
        Ok(ReleaseDownloadResult {
            status: status.as_str().to_string(),
            detail,
        })
    }
    #[cfg(not(feature = "server"))]
    {
        let _ = rg_mbid;
        unreachable!()
    }
}

/// Check a followed artist for new releases now.
#[post("/api/artists/check", auth: AuthSession)]
pub async fn check_followed_artist(mbid: String) -> Result<CheckResult, ServerFnError> {
    #[cfg(feature = "server")]
    {
        let summary = crate::artist_monitor::check_artist_now(&auth.0.sub, &mbid)
            .await
            .map_err(server_error);
        logic::cache_invalidate(&mbid);
        let s = summary?;
        Ok(CheckResult {
            already_running: s.already_running,
            added_new: s.added_new as u32,
            added_upcoming: s.added_upcoming as u32,
            became_new: s.became_new as u32,
            queued: s.queued as u32,
            manual: s.manual as u32,
            failed: s.failed as u32,
        })
    }
    #[cfg(not(feature = "server"))]
    {
        let _ = mbid;
        unreachable!()
    }
}
