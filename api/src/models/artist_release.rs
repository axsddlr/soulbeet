use serde::{Deserialize, Serialize};
use shared::metadata::ReleaseKind;

#[cfg(feature = "server")]
use sqlx::SqlitePool;

/// Where a followed artist's release group stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReleaseStatus {
    /// Seen at the first check, or not monitored (a single the user did not
    /// opt into). Never shown as new.
    Known,
    /// New, but its first release date is still ahead.
    Upcoming,
    /// New and released.
    New,
    /// A download was enqueued.
    Queued,
    /// The best Soulseek match was too weak; the user must pick.
    Manual,
    /// The download could not start; `status_detail` says why.
    Failed,
    /// The user dismissed it.
    Dismissed,
}

impl ReleaseStatus {
    pub const ALL: [ReleaseStatus; 7] = [
        ReleaseStatus::Known,
        ReleaseStatus::Upcoming,
        ReleaseStatus::New,
        ReleaseStatus::Queued,
        ReleaseStatus::Manual,
        ReleaseStatus::Failed,
        ReleaseStatus::Dismissed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ReleaseStatus::Known => "known",
            ReleaseStatus::Upcoming => "upcoming",
            ReleaseStatus::New => "new",
            ReleaseStatus::Queued => "queued",
            ReleaseStatus::Manual => "manual",
            ReleaseStatus::Failed => "failed",
            ReleaseStatus::Dismissed => "dismissed",
        }
    }

    pub fn parse(value: &str) -> Option<ReleaseStatus> {
        Self::ALL.into_iter().find(|s| s.as_str() == value)
    }
}

/// The stored form of a release kind: "album", "ep" or "single".
pub fn kind_as_str(kind: ReleaseKind) -> &'static str {
    match kind {
        ReleaseKind::Album => "album",
        ReleaseKind::Ep => "ep",
        ReleaseKind::Single => "single",
    }
}

pub fn parse_kind(value: &str) -> Option<ReleaseKind> {
    match value {
        "album" => Some(ReleaseKind::Album),
        "ep" => Some(ReleaseKind::Ep),
        "single" => Some(ReleaseKind::Single),
        _ => None,
    }
}

/// A release group seen for a followed artist. `kind` and `status` hold
/// the strings of [`kind_as_str`] and [`ReleaseStatus::as_str`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "server", derive(sqlx::FromRow))]
pub struct ArtistRelease {
    pub user_id: String,
    pub artist_mbid: String,
    pub release_group_mbid: String,
    pub title: String,
    pub kind: String,
    pub first_release_date: Option<String>,
    pub status: String,
    pub status_detail: Option<String>,
    pub discovered_at: String,
    pub updated_at: String,
}

impl ArtistRelease {
    pub fn status(&self) -> Option<ReleaseStatus> {
        ReleaseStatus::parse(&self.status)
    }

    pub fn kind(&self) -> Option<ReleaseKind> {
        parse_kind(&self.kind)
    }
}

#[cfg(feature = "server")]
impl ArtistRelease {
    /// Records a release group. One the user already has (under any
    /// followed artist) is left as it is.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert<'e, E>(
        executor: E,
        user_id: &str,
        artist_mbid: &str,
        release_group_mbid: &str,
        title: &str,
        kind: ReleaseKind,
        first_release_date: Option<&str>,
        status: ReleaseStatus,
    ) -> Result<(), String>
    where
        E: sqlx::SqliteExecutor<'e>,
    {
        sqlx::query(
            "INSERT INTO artist_releases
                (user_id, artist_mbid, release_group_mbid, title, kind, first_release_date, status)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(user_id, release_group_mbid) DO NOTHING",
        )
        .bind(user_id)
        .bind(artist_mbid)
        .bind(release_group_mbid)
        .bind(title)
        .bind(kind_as_str(kind))
        .bind(first_release_date)
        .bind(status.as_str())
        .execute(executor)
        .await
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub async fn get(
        pool: &SqlitePool,
        user_id: &str,
        release_group_mbid: &str,
    ) -> Result<Option<ArtistRelease>, String> {
        sqlx::query_as::<_, ArtistRelease>(
            "SELECT * FROM artist_releases WHERE user_id = ? AND release_group_mbid = ?",
        )
        .bind(user_id)
        .bind(release_group_mbid)
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())
    }

    /// Every release the user has, under any followed artist.
    pub async fn all_for_user(
        pool: &SqlitePool,
        user_id: &str,
    ) -> Result<Vec<ArtistRelease>, String> {
        sqlx::query_as::<_, ArtistRelease>("SELECT * FROM artist_releases WHERE user_id = ?")
            .bind(user_id)
            .fetch_all(pool)
            .await
            .map_err(|e| e.to_string())
    }

    /// The artist's releases, newest first; undated ones last.
    pub async fn releases_for_artist(
        pool: &SqlitePool,
        user_id: &str,
        artist_mbid: &str,
    ) -> Result<Vec<ArtistRelease>, String> {
        sqlx::query_as::<_, ArtistRelease>(
            "SELECT * FROM artist_releases WHERE user_id = ? AND artist_mbid = ?
             ORDER BY first_release_date IS NULL, first_release_date DESC, title",
        )
        .bind(user_id)
        .bind(artist_mbid)
        .fetch_all(pool)
        .await
        .map_err(|e| e.to_string())
    }

    /// The user's releases in any of `statuses`, across followed artists,
    /// newest first; undated ones last. Empty `statuses` gives none.
    pub async fn releases_by_status(
        pool: &SqlitePool,
        user_id: &str,
        statuses: &[ReleaseStatus],
    ) -> Result<Vec<ArtistRelease>, String> {
        if statuses.is_empty() {
            return Ok(Vec::new());
        }
        let marks = vec!["?"; statuses.len()].join(", ");
        let sql = format!(
            "SELECT * FROM artist_releases WHERE user_id = ? AND status IN ({marks})
             ORDER BY first_release_date IS NULL, first_release_date DESC, title"
        );
        let mut query = sqlx::query_as::<_, ArtistRelease>(&sql).bind(user_id);
        for status in statuses {
            query = query.bind(status.as_str());
        }
        query.fetch_all(pool).await.map_err(|e| e.to_string())
    }

    /// Sets a release's status and detail. `false` when there is no such
    /// release.
    pub async fn set_status<'e, E>(
        executor: E,
        user_id: &str,
        release_group_mbid: &str,
        status: ReleaseStatus,
        detail: Option<&str>,
    ) -> Result<bool, String>
    where
        E: sqlx::SqliteExecutor<'e>,
    {
        let done = sqlx::query(
            "UPDATE artist_releases SET status = ?, status_detail = ?, updated_at = datetime('now')
             WHERE user_id = ? AND release_group_mbid = ?",
        )
        .bind(status.as_str())
        .bind(detail)
        .bind(user_id)
        .bind(release_group_mbid)
        .execute(executor)
        .await
        .map_err(|e| e.to_string())?;
        Ok(done.rows_affected() > 0)
    }

    /// Moves a release to `status`, updating its first release date too.
    pub async fn transition<'e, E>(
        executor: E,
        user_id: &str,
        release_group_mbid: &str,
        status: ReleaseStatus,
        first_release_date: Option<&str>,
    ) -> Result<(), String>
    where
        E: sqlx::SqliteExecutor<'e>,
    {
        sqlx::query(
            "UPDATE artist_releases
             SET status = ?, status_detail = NULL, first_release_date = ?,
                 updated_at = datetime('now')
             WHERE user_id = ? AND release_group_mbid = ?",
        )
        .bind(status.as_str())
        .bind(first_release_date)
        .bind(user_id)
        .bind(release_group_mbid)
        .execute(executor)
        .await
        .map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[cfg(all(test, feature = "server"))]
mod tests {
    use super::*;
    use crate::models::followed_artist::tests::pool_with_user;
    use crate::models::followed_artist::FollowedArtist;

    #[test]
    fn status_strings_round_trip() {
        for status in ReleaseStatus::ALL {
            assert_eq!(ReleaseStatus::parse(status.as_str()), Some(status));
        }
        assert_eq!(ReleaseStatus::parse("bogus"), None);
        for kind in [ReleaseKind::Album, ReleaseKind::Ep, ReleaseKind::Single] {
            assert_eq!(parse_kind(kind_as_str(kind)), Some(kind));
        }
    }

    #[tokio::test]
    async fn releases_by_status_filters_and_set_status_updates() {
        let pool = pool_with_user("u1").await;
        FollowedArtist::follow(&pool, "u1", "a1", "A")
            .await
            .unwrap();
        for (rg, date, status) in [
            ("rg1", Some("2020-01-01"), ReleaseStatus::New),
            ("rg2", Some("2021-01-01"), ReleaseStatus::Known),
            ("rg3", None, ReleaseStatus::Failed),
        ] {
            ArtistRelease::insert(&pool, "u1", "a1", rg, "T", ReleaseKind::Ep, date, status)
                .await
                .unwrap();
        }
        let rows = ArtistRelease::releases_by_status(
            &pool,
            "u1",
            &[ReleaseStatus::New, ReleaseStatus::Failed],
        )
        .await
        .unwrap();
        let ids: Vec<&str> = rows.iter().map(|r| r.release_group_mbid.as_str()).collect();
        assert_eq!(ids, vec!["rg1", "rg3"]);
        assert!(ArtistRelease::releases_by_status(&pool, "u1", &[])
            .await
            .unwrap()
            .is_empty());

        assert!(
            ArtistRelease::set_status(&pool, "u1", "rg1", ReleaseStatus::Failed, Some("why"))
                .await
                .unwrap()
        );
        let row = ArtistRelease::get(&pool, "u1", "rg1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.status(), Some(ReleaseStatus::Failed));
        assert_eq!(row.status_detail.as_deref(), Some("why"));
        assert_eq!(row.kind(), Some(ReleaseKind::Ep));
        assert!(
            !ArtistRelease::set_status(&pool, "u1", "nope", ReleaseStatus::New, None)
                .await
                .unwrap()
        );
    }
}
