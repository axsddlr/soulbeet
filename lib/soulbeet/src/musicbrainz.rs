use musicbrainz_rs::{
    entity::{
        artist::{Artist, ArtistSearchQuery},
        artist_credit::ArtistCredit,
        recording::{Recording, RecordingSearchQuery},
        release::{Release, ReleaseStatus},
        release_group::{ReleaseGroup, ReleaseGroupPrimaryType, ReleaseGroupSearchQuery},
    },
    Browse, Fetch, MusicBrainzClient, Search,
};
use shared::metadata::{
    Album, AlbumWithTracks, ArtistSummary, ReleaseGroupSummary, SearchResult, Track,
};
use std::{collections::HashSet, future::Future, sync::OnceLock, time::Duration};
use tokio::time::sleep;
use tracing::{info, warn};

/// Timeout for individual MusicBrainz requests (15 seconds)
const REQUEST_TIMEOUT_SECS: u64 = 15;

/// Maximum retries for transient errors
const MAX_RETRIES: u32 = 3;

/// Base delay for exponential backoff (milliseconds)
const BASE_DELAY_MS: u64 = 500;

/// Maximum backoff delay cap (milliseconds)
const MAX_BACKOFF_MS: u64 = 5000;

// This ensures the client is initialized only once with a proper user agent.
fn musicbrainz_client() -> &'static MusicBrainzClient {
    static CLIENT: OnceLock<MusicBrainzClient> = OnceLock::new();
    CLIENT.get_or_init(|| {
        let version = env!("CARGO_PKG_VERSION");
        let mut client = MusicBrainzClient::new(&format!(
            "Soulbeet/{version} ( https://github.com/terry90/soulbeet )"
        ))
        .expect("Failed to create MusicBrainz client - invalid user agent format");
        // MUSICBRAINZ_HOST points the client at a MusicBrainz mirror
        // (host[:port], no scheme). Mirrors the `musicbrainz.host` option
        // beets exposes for the same purpose.
        if let Ok(host) = std::env::var("MUSICBRAINZ_HOST") {
            let host = host.trim();
            if !host.is_empty() {
                client.musicbrainz_domain = host.to_string();
            }
        }
        client
    })
}

/// Formats the artist credits into a single, comma-separated string.
fn format_artist_credit(credits: &Option<Vec<ArtistCredit>>) -> String {
    credits
        .as_ref()
        .map(|credits| {
            credits
                .iter()
                .map(|credit| credit.name.clone())
                .collect::<Vec<String>>()
                .join(", ")
        })
        .unwrap_or_else(|| "Unknown Artist".to_string())
}

/// Formats a duration from milliseconds to a MM:SS string.
fn format_duration(duration_ms: &Option<u32>) -> Option<String> {
    duration_ms.map(|ms| {
        let total_seconds = ms / 1000;
        let minutes = total_seconds / 60;
        let seconds = total_seconds % 60;
        format!("{minutes:02}:{seconds:02}")
    })
}

/// Check if an error is retryable (transient network/server issues)
fn is_retryable_error(error: &musicbrainz_rs::Error) -> bool {
    let error_str = format!("{:?}", error);
    let error_lower = error_str.to_lowercase();

    // Retry on timeout, connection, and 5xx server errors
    if error_lower.contains("timeout")
        || error_lower.contains("connection")
        || error_lower.contains("timed out")
        || error_lower.contains("503")
        || error_lower.contains("502")
        || error_lower.contains("500")
        || error_lower.contains("429") // Rate limited - should retry after backoff
        || error_lower.contains("service unavailable")
    {
        return true;
    }

    // Don't retry client errors (4xx except 429)
    if error_lower.contains("400")
        || error_lower.contains("401")
        || error_lower.contains("403")
        || error_lower.contains("404")
        || error_lower.contains("bad request")
        || error_lower.contains("not found")
        || error_lower.contains("unauthorized")
    {
        return false;
    }

    // Default to retrying for unknown errors (network issues, etc.)
    true
}

/// Retries an async operation with exponential backoff and request timeout.
/// Only retries transient errors (network issues, timeouts, 5xx responses).
/// Does NOT retry client errors (4xx) or permanent failures.
async fn with_retry<T, F, Fut>(
    operation_name: &str,
    mut operation: F,
) -> Result<T, musicbrainz_rs::Error>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, musicbrainz_rs::Error>>,
{
    let mut last_error = None;

    for attempt in 0..MAX_RETRIES {
        // Respect MusicBrainz rate limit (1 req/sec)
        crate::http::mb_rate_limit().await;

        // Apply timeout to each request
        let result =
            tokio::time::timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS), operation()).await;

        match result {
            Ok(Ok(value)) => return Ok(value),
            Ok(Err(e)) => {
                // Check if this error is retryable
                if !is_retryable_error(&e) {
                    warn!(
                        "{} failed with non-retryable error: {:?}",
                        operation_name, e
                    );
                    return Err(e);
                }

                last_error = Some(e);
                if attempt < MAX_RETRIES - 1 {
                    let delay = std::cmp::min(BASE_DELAY_MS * 2u64.pow(attempt), MAX_BACKOFF_MS);
                    warn!(
                        "{} failed (attempt {}/{}), retrying in {}ms: {:?}",
                        operation_name,
                        attempt + 1,
                        MAX_RETRIES,
                        delay,
                        last_error
                    );
                    sleep(Duration::from_millis(delay)).await;
                }
            }
            Err(_timeout) => {
                // Request timed out
                warn!(
                    "{} timed out after {}s (attempt {}/{})",
                    operation_name,
                    REQUEST_TIMEOUT_SECS,
                    attempt + 1,
                    MAX_RETRIES
                );
                last_error = Some(musicbrainz_rs::Error::MaxRetriesExceeded);
                if attempt < MAX_RETRIES - 1 {
                    let delay = std::cmp::min(BASE_DELAY_MS * 2u64.pow(attempt), MAX_BACKOFF_MS);
                    sleep(Duration::from_millis(delay)).await;
                }
            }
        }
    }

    match last_error {
        Some(e) => Err(e),
        None => Err(musicbrainz_rs::Error::MaxRetriesExceeded),
    }
}

/// An enumeration to specify the type of search.
#[derive(Debug)]
pub enum SearchType {
    Track,
    Album,
}

/// Performs a refined search for music, prioritizing canonical releases.
pub async fn search(
    artist: &Option<String>,
    query: &str,
    search_type: SearchType,
    limit: u8,
) -> Result<Vec<SearchResult>, musicbrainz_rs::Error> {
    let client = musicbrainz_client();
    let mut results = Vec::new();

    info!(
        "Starting {:?} search for query: '{}', artist: '{:?}'",
        search_type, query, artist
    );

    match search_type {
        SearchType::Track => {
            let search_results = with_retry("MusicBrainz track search", || {
                let mut recording_query = RecordingSearchQuery::query_builder();
                if let Some(ref artist) = artist {
                    recording_query.artist_name(artist).and();
                }
                let search_query = recording_query.recording(query).build();
                async move {
                    Recording::search(search_query)
                        .limit(limit)
                        .with_releases()
                        .with_ratings()
                        .execute_with_client(client)
                        .await
                }
            })
            .await?;

            // Sort by rating (descending) - higher rated recordings first
            let mut recordings: Vec<_> = search_results.entities;
            recordings.sort_by(|a, b| {
                let a_rating = a.rating.as_ref().and_then(|r| r.value).unwrap_or(0.0);
                let b_rating = b.rating.as_ref().and_then(|r| r.value).unwrap_or(0.0);
                b_rating
                    .partial_cmp(&a_rating)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            let mut unique_tracks = HashSet::new();

            for recording in recordings {
                let artist_name = format_artist_credit(&recording.artist_credit);
                let album_title = recording
                    .releases
                    .as_ref()
                    .and_then(|r| r.first())
                    .map(|r| r.title.clone())
                    .unwrap_or_default();

                // Use a combination of title, artist, and album to define a unique track
                let key = (
                    recording.title.to_lowercase(),
                    artist_name.to_lowercase(),
                    album_title.to_lowercase(),
                );

                if !unique_tracks.contains(&key) {
                    let first_release = recording.releases.as_ref().and_then(|r| r.first());
                    let track = Track {
                        id: recording.id.clone(),
                        title: recording.title.clone(),
                        artist: artist_name.clone(),
                        album_id: first_release.map(|release| release.id.clone()),
                        album_title: first_release.map(|r| r.title.clone()),
                        release_date: first_release.and_then(|r| r.date.clone().map(|d| d.0)),
                        duration: format_duration(&recording.length),
                        mbid: Some(recording.id.clone()),
                        release_mbid: first_release.map(|r| r.id.clone()),
                    };
                    unique_tracks.insert(key);
                    results.push(SearchResult::Track(track));
                }
            }
        }
        SearchType::Album => {
            let search_results = with_retry("MusicBrainz album search", || {
                let mut album_query = ReleaseGroupSearchQuery::query_builder();
                if let Some(ref artist) = artist {
                    album_query.artist(artist).and();
                }
                let search_query = album_query.release_group(query).build();
                async move {
                    ReleaseGroup::search(search_query)
                        .limit(limit)
                        .with_releases()
                        .with_ratings()
                        .execute_with_client(client)
                        .await
                }
            })
            .await?;

            // Sort by rating (descending) - higher rated albums first
            let mut release_groups: Vec<_> = search_results.entities;
            release_groups.sort_by(|a, b| {
                let a_rating = a.rating.as_ref().and_then(|r| r.value).unwrap_or(0.0);
                let b_rating = b.rating.as_ref().and_then(|r| r.value).unwrap_or(0.0);
                b_rating
                    .partial_cmp(&a_rating)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            for release_group in release_groups {
                if release_group.primary_type != Some(ReleaseGroupPrimaryType::Album)
                    && release_group.primary_type != Some(ReleaseGroupPrimaryType::Ep)
                {
                    continue;
                }

                if let Some(best_release) = release_group
                    .releases
                    .as_deref()
                    .and_then(earliest_official_release)
                {
                    // If no official release was found, take the first one available
                    let final_release = best_release.clone();

                    results.push(SearchResult::Album(Album {
                        id: final_release.id.clone(),
                        title: release_group.title.clone(),
                        artist: format_artist_credit(&release_group.artist_credit),
                        release_date: final_release.date.as_ref().map(|d| d.0.clone()),
                        mbid: Some(final_release.id.clone()),
                        cover_url: None,
                    }));
                }
            }
        }
    }

    Ok(results)
}

/// Picks the release to download from a release group's releases: the
/// earliest-dated Official one (undated releases sort first, ties keep the
/// first listed). Shared by album search and artist monitoring.
fn earliest_official_release(releases: &[Release]) -> Option<&Release> {
    pick_earliest_official(
        releases,
        |r| r.status == Some(ReleaseStatus::Official),
        |r| r.date.as_ref().map(|d| d.0.as_str()),
    )
}

/// Selection rule behind [`earliest_official_release`], independent of the
/// MusicBrainz entity types so it can be unit tested.
fn pick_earliest_official<'a, T>(
    items: &'a [T],
    is_official: impl Fn(&T) -> bool,
    date: impl for<'b> Fn(&'b T) -> Option<&'b str>,
) -> Option<&'a T> {
    items
        .iter()
        .filter(|r| is_official(r))
        .min_by_key(|r| date(r))
}

/// Page size for release group browsing (MusicBrainz maximum).
const BROWSE_PAGE_SIZE: u8 = 100;

/// Upper bound on release groups collected for one artist.
const MAX_RELEASE_GROUPS: usize = 1000;

/// Whether another browse page is needed, given how many entries have been
/// accumulated, the size of the page just received and the reported total.
fn needs_more_pages(accumulated: usize, last_page_len: usize, total: usize, cap: usize) -> bool {
    last_page_len > 0 && accumulated < total.min(cap)
}

/// Sorts newest first by first release date; undated entries go last.
fn sort_release_groups(groups: &mut [ReleaseGroupSummary]) {
    groups.sort_by(
        |a, b| match (&a.first_release_date, &b.first_release_date) {
            (Some(x), Some(y)) => y.cmp(x),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        },
    );
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

fn artist_summary(a: &Artist) -> ArtistSummary {
    ArtistSummary {
        mbid: a.id.clone(),
        name: a.name.clone(),
        disambiguation: non_empty(&a.disambiguation),
        country: a.country.as_deref().and_then(non_empty),
        kind: a.artist_type.as_ref().map(|t| format!("{t:?}")),
    }
}

fn release_group_summary(rg: &ReleaseGroup) -> ReleaseGroupSummary {
    ReleaseGroupSummary {
        mbid: rg.id.clone(),
        title: rg.title.clone(),
        primary_type: rg.primary_type.as_ref().map(|t| match t {
            ReleaseGroupPrimaryType::Ep => "EP".to_string(),
            other => format!("{other:?}"),
        }),
        secondary_types: rg
            .secondary_types
            .iter()
            .map(|t| format!("{t:?}"))
            .collect(),
        first_release_date: rg.first_release_date.as_ref().and_then(|d| non_empty(&d.0)),
    }
}

/// Searches MusicBrainz artists by name.
pub async fn search_artists(
    query: &str,
    limit: u8,
) -> Result<Vec<ArtistSummary>, musicbrainz_rs::Error> {
    let client = musicbrainz_client();
    let results = with_retry("MusicBrainz artist search", || {
        let search_query = ArtistSearchQuery::query_builder().artist(query).build();
        async move {
            Artist::search(search_query)
                .limit(limit)
                .execute_with_client(client)
                .await
        }
    })
    .await?;
    Ok(results.entities.iter().map(artist_summary).collect())
}

/// Fetches a single artist by MBID.
pub async fn get_artist(mbid: &str) -> Result<ArtistSummary, musicbrainz_rs::Error> {
    let client = musicbrainz_client();
    let artist = with_retry("MusicBrainz artist fetch", || async {
        Artist::fetch().id(mbid).execute_with_client(client).await
    })
    .await?;
    Ok(artist_summary(&artist))
}

/// Lists all Album/EP/Single release groups of an artist, newest first.
/// Pages through the browse endpoint (100 per page, capped at 1000 entries);
/// every page is rate limited and retried.
pub async fn artist_release_groups(
    artist_mbid: &str,
) -> Result<Vec<ReleaseGroupSummary>, musicbrainz_rs::Error> {
    let client = musicbrainz_client();
    // `by_artist` appends its argument verbatim to the query string, which
    // lets us add the type filter (%7C is an encoded `|`).
    let filter = format!("{artist_mbid}&type=album%7Cep%7Csingle");
    let mut groups: Vec<ReleaseGroupSummary> = Vec::new();

    loop {
        let offset = groups.len();
        let page = with_retry("MusicBrainz release group browse", || {
            let filter = filter.clone();
            async move {
                ReleaseGroup::browse()
                    .by_artist(&filter)
                    .limit(BROWSE_PAGE_SIZE)
                    .offset(offset as u16)
                    .execute_with_client(client)
                    .await
            }
        })
        .await?;

        let page_len = page.entities.len();
        groups.extend(page.entities.iter().map(release_group_summary));
        let total = usize::try_from(page.count).unwrap_or(0);
        if !needs_more_pages(groups.len(), page_len, total, MAX_RELEASE_GROUPS) {
            break;
        }
    }

    groups.truncate(MAX_RELEASE_GROUPS);
    sort_release_groups(&mut groups);
    Ok(groups)
}

/// The release id to download for a release group (earliest Official
/// release, same rule as album search), if it has one.
pub async fn release_group_release(
    release_group_mbid: &str,
) -> Result<Option<String>, musicbrainz_rs::Error> {
    let client = musicbrainz_client();
    let rg = with_retry("MusicBrainz release group fetch", || async {
        ReleaseGroup::fetch()
            .id(release_group_mbid)
            .with_releases()
            .execute_with_client(client)
            .await
    })
    .await?;
    Ok(rg
        .releases
        .as_deref()
        .and_then(earliest_official_release)
        .map(|r| r.id.clone()))
}

/// Fetches a release (album) by its ID and returns it with its full tracklist.
pub async fn find_album(release_id: &str) -> Result<AlbumWithTracks, musicbrainz_rs::Error> {
    let client = musicbrainz_client();

    // Fetch the release with recordings (tracks) and artist credits for the tracks.
    let release = with_retry("MusicBrainz album fetch", || async {
        Release::fetch()
            .id(release_id)
            .with_recordings()
            .with_artist_credits()
            .execute_with_client(client)
            .await
    })
    .await?;

    let mut tracks = Vec::new();

    // A release contains media (like CD 1, CD 2), and each medium has tracks.
    if let Some(media) = &release.media {
        for medium in media {
            if let Some(release_tracks) = &medium.tracks {
                for track in release_tracks {
                    if let Some(recording) = &track.recording {
                        tracks.push(Track {
                            id: recording.id.clone(),
                            title: recording.title.clone(),
                            artist: format_artist_credit(&recording.artist_credit),
                            album_id: Some(release.id.clone()),
                            album_title: Some(release.title.clone()),
                            release_date: release.date.as_ref().map(|d| d.0.clone()),
                            duration: format_duration(&recording.length),
                            mbid: Some(recording.id.clone()),
                            release_mbid: Some(release.id.clone()),
                        });
                    }
                }
            }
        }
    }

    // First, create the standalone Album object.
    let album = Album {
        id: release.id.clone(),
        title: release.title,
        artist: format_artist_credit(&release.artist_credit),
        release_date: release.date.map(|d| d.0),
        mbid: Some(release.id),
        cover_url: None,
    };

    let album_with_tracks = AlbumWithTracks { album, tracks };

    Ok(album_with_tracks)
}

pub struct MusicBrainzProvider;

impl MusicBrainzProvider {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MusicBrainzProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl crate::MetadataProvider for MusicBrainzProvider {
    fn id(&self) -> &'static str {
        "musicbrainz"
    }

    fn name(&self) -> &'static str {
        "MusicBrainz"
    }

    async fn search_albums(
        &self,
        artist: Option<&str>,
        query: &str,
        limit: usize,
    ) -> crate::error::Result<Vec<SearchResult>> {
        let artist_opt = artist.map(String::from);
        search(&artist_opt, query, SearchType::Album, limit.min(100) as u8)
            .await
            .map_err(|e| crate::error::SoulseekError::Api {
                status: 500,
                message: e.to_string(),
            })
    }

    async fn search_tracks(
        &self,
        artist: Option<&str>,
        query: &str,
        limit: usize,
    ) -> crate::error::Result<Vec<SearchResult>> {
        let artist_opt = artist.map(String::from);
        search(&artist_opt, query, SearchType::Track, limit.min(100) as u8)
            .await
            .map_err(|e| crate::error::SoulseekError::Api {
                status: 500,
                message: e.to_string(),
            })
    }

    async fn get_album(&self, id: &str) -> crate::error::Result<AlbumWithTracks> {
        find_album(id)
            .await
            .map_err(|e| crate::error::SoulseekError::Api {
                status: 500,
                message: e.to_string(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rg(date: Option<&str>) -> ReleaseGroupSummary {
        ReleaseGroupSummary {
            mbid: date.unwrap_or("none").into(),
            title: "t".into(),
            primary_type: Some("Album".into()),
            secondary_types: vec![],
            first_release_date: date.map(Into::into),
        }
    }

    #[test]
    fn paging_stops_at_total() {
        assert!(needs_more_pages(100, 100, 250, 1000));
        assert!(needs_more_pages(200, 100, 250, 1000));
        assert!(!needs_more_pages(250, 50, 250, 1000));
        assert!(!needs_more_pages(100, 100, 100, 1000));
    }

    #[test]
    fn paging_stops_at_cap() {
        assert!(needs_more_pages(900, 100, 5000, 1000));
        assert!(!needs_more_pages(1000, 100, 5000, 1000));
    }

    #[test]
    fn paging_stops_on_empty_page() {
        assert!(!needs_more_pages(100, 0, 500, 1000));
        assert!(!needs_more_pages(0, 0, 0, 1000));
    }

    #[test]
    fn sorts_newest_first_with_undated_last() {
        let mut v = vec![
            rg(None),
            rg(Some("1999-05-01")),
            rg(Some("2010")),
            rg(Some("2010-03-04")),
        ];
        sort_release_groups(&mut v);
        let order: Vec<_> = v.iter().map(|g| g.mbid.as_str()).collect();
        assert_eq!(order, ["2010-03-04", "2010", "1999-05-01", "none"]);
    }

    // (id, official, date)
    type R = (&'static str, bool, Option<&'static str>);

    fn pick(items: &[R]) -> Option<&'static str> {
        pick_earliest_official(items, |r| r.1, |r| r.2).map(|r| r.0)
    }

    #[test]
    fn picks_earliest_official_release() {
        let items: [R; 3] = [
            ("late", true, Some("2005-01-01")),
            ("early", true, Some("2001-06-01")),
            ("bootleg", false, Some("1990-01-01")),
        ];
        assert_eq!(pick(&items), Some("early"));
    }

    #[test]
    fn release_pick_ignores_unofficial_and_handles_empty() {
        assert_eq!(pick(&[("promo", false, Some("2000"))]), None);
        assert_eq!(pick(&[]), None);
    }

    #[test]
    fn release_pick_matches_search_rule_for_undated_and_ties() {
        // Undated sorts before dated; ties keep the first listed.
        let items: [R; 3] = [
            ("dated", true, Some("2000")),
            ("undated", true, None),
            ("undated2", true, None),
        ];
        assert_eq!(pick(&items), Some("undated"));
    }
}
