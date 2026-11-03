use serde::{Deserialize, Serialize};

#[cfg(feature = "server")]
use sqlx::SqlitePool;

/// An artist a user follows for new releases.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "server", derive(sqlx::FromRow))]
pub struct FollowedArtist {
    pub user_id: String,
    pub artist_mbid: String,
    pub name: String,
    pub include_singles: bool,
    pub auto_download: bool,
    pub folder_id: Option<String>,
    pub created_at: String,
    /// When the last check ran; `None` until the first successful one.
    pub last_checked_at: Option<String>,
    /// Why the last check failed; cleared by a successful check.
    pub last_error: Option<String>,
}

#[cfg(feature = "server")]
impl FollowedArtist {
    /// Follows `artist_mbid` for `user_id`. An existing follow is returned
    /// as it is, its settings untouched.
    pub async fn follow(
        pool: &SqlitePool,
        user_id: &str,
        artist_mbid: &str,
        name: &str,
    ) -> Result<FollowedArtist, String> {
        sqlx::query(
            "INSERT INTO followed_artists (user_id, artist_mbid, name) VALUES (?, ?, ?)
             ON CONFLICT(user_id, artist_mbid) DO NOTHING",
        )
        .bind(user_id)
        .bind(artist_mbid)
        .bind(name)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;
        Self::get(pool, user_id, artist_mbid)
            .await?
            .ok_or_else(|| "Follow vanished after insert".to_string())
    }

    /// Unfollows, deleting the follow's releases too. `false` when the user
    /// did not follow the artist.
    pub async fn unfollow(
        pool: &SqlitePool,
        user_id: &str,
        artist_mbid: &str,
    ) -> Result<bool, String> {
        let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
        // Explicit so it does not depend on the foreign_keys pragma.
        sqlx::query("DELETE FROM artist_releases WHERE user_id = ? AND artist_mbid = ?")
            .bind(user_id)
            .bind(artist_mbid)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        let done =
            sqlx::query("DELETE FROM followed_artists WHERE user_id = ? AND artist_mbid = ?")
                .bind(user_id)
                .bind(artist_mbid)
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
        tx.commit().await.map_err(|e| e.to_string())?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn get(
        pool: &SqlitePool,
        user_id: &str,
        artist_mbid: &str,
    ) -> Result<Option<FollowedArtist>, String> {
        sqlx::query_as::<_, FollowedArtist>(
            "SELECT * FROM followed_artists WHERE user_id = ? AND artist_mbid = ?",
        )
        .bind(user_id)
        .bind(artist_mbid)
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())
    }

    /// The user's follows, by name.
    pub async fn list_for_user(
        pool: &SqlitePool,
        user_id: &str,
    ) -> Result<Vec<FollowedArtist>, String> {
        sqlx::query_as::<_, FollowedArtist>(
            "SELECT * FROM followed_artists WHERE user_id = ?
             ORDER BY name COLLATE NOCASE, artist_mbid",
        )
        .bind(user_id)
        .fetch_all(pool)
        .await
        .map_err(|e| e.to_string())
    }

    /// `false` when the user does not follow the artist.
    pub async fn update_settings(
        pool: &SqlitePool,
        user_id: &str,
        artist_mbid: &str,
        include_singles: bool,
        auto_download: bool,
        folder_id: Option<&str>,
    ) -> Result<bool, String> {
        let done = sqlx::query(
            "UPDATE followed_artists SET include_singles = ?, auto_download = ?, folder_id = ?
             WHERE user_id = ? AND artist_mbid = ?",
        )
        .bind(include_singles)
        .bind(auto_download)
        .bind(folder_id)
        .bind(user_id)
        .bind(artist_mbid)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;
        Ok(done.rows_affected() > 0)
    }

    /// Follows of every user never checked, or last checked at least
    /// `older_than_hours` ago. Never-checked ones come first.
    pub async fn due_for_check(
        pool: &SqlitePool,
        older_than_hours: i64,
    ) -> Result<Vec<FollowedArtist>, String> {
        sqlx::query_as::<_, FollowedArtist>(
            "SELECT * FROM followed_artists
             WHERE last_checked_at IS NULL OR last_checked_at <= datetime('now', ?)
             ORDER BY last_checked_at IS NOT NULL, last_checked_at, created_at",
        )
        .bind(format!("-{older_than_hours} hours"))
        .fetch_all(pool)
        .await
        .map_err(|e| e.to_string())
    }

    /// Records a check that failed with `error`. `last_checked_at` moves
    /// only when the follow was checked before, so a failed first check is
    /// retried as a baseline.
    pub async fn record_check_failure(
        pool: &SqlitePool,
        user_id: &str,
        artist_mbid: &str,
        error: &str,
    ) -> Result<(), String> {
        sqlx::query(
            "UPDATE followed_artists
             SET last_error = ?,
                 last_checked_at = CASE WHEN last_checked_at IS NULL THEN NULL
                                        ELSE datetime('now') END
             WHERE user_id = ? AND artist_mbid = ?",
        )
        .bind(error)
        .bind(user_id)
        .bind(artist_mbid)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Records a successful check: stamps `last_checked_at`, clears
    /// `last_error`.
    pub async fn record_check_success<'e, E>(
        executor: E,
        user_id: &str,
        artist_mbid: &str,
    ) -> Result<(), String>
    where
        E: sqlx::SqliteExecutor<'e>,
    {
        sqlx::query(
            "UPDATE followed_artists SET last_checked_at = datetime('now'), last_error = NULL
             WHERE user_id = ? AND artist_mbid = ?",
        )
        .bind(user_id)
        .bind(artist_mbid)
        .execute(executor)
        .await
        .map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[cfg(all(test, feature = "server"))]
pub(crate) mod tests {
    use super::*;
    use crate::models::artist_release::{ArtistRelease, ReleaseStatus};
    use shared::metadata::ReleaseKind;
    use sqlx::sqlite::SqlitePoolOptions;

    pub(crate) async fn pool_with_user(user_id: &str) -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query("INSERT INTO users (id, username, password_hash) VALUES (?, ?, 'x')")
            .bind(user_id)
            .bind(format!("name-{user_id}"))
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    #[tokio::test]
    async fn following_twice_keeps_the_settings() {
        let pool = pool_with_user("u1").await;
        let first = FollowedArtist::follow(&pool, "u1", "a1", "Artist")
            .await
            .unwrap();
        assert!(!first.include_singles && !first.auto_download);
        assert!(first.last_checked_at.is_none());
        FollowedArtist::update_settings(&pool, "u1", "a1", true, true, Some("f1"))
            .await
            .unwrap();
        let again = FollowedArtist::follow(&pool, "u1", "a1", "Other name")
            .await
            .unwrap();
        assert!(again.include_singles);
        assert!(again.auto_download);
        assert_eq!(again.folder_id.as_deref(), Some("f1"));
        assert_eq!(again.name, "Artist");
        assert_eq!(
            FollowedArtist::list_for_user(&pool, "u1")
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn update_settings_reports_a_missing_follow() {
        let pool = pool_with_user("u1").await;
        assert!(
            !FollowedArtist::update_settings(&pool, "u1", "nope", true, false, None)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn unfollow_removes_the_releases() {
        let pool = pool_with_user("u1").await;
        FollowedArtist::follow(&pool, "u1", "a1", "A")
            .await
            .unwrap();
        FollowedArtist::follow(&pool, "u1", "a2", "B")
            .await
            .unwrap();
        for (artist, rg) in [("a1", "rg1"), ("a1", "rg2"), ("a2", "rg3")] {
            ArtistRelease::insert(
                &pool,
                "u1",
                artist,
                rg,
                "T",
                ReleaseKind::Album,
                None,
                ReleaseStatus::Known,
            )
            .await
            .unwrap();
        }
        assert!(FollowedArtist::unfollow(&pool, "u1", "a1").await.unwrap());
        assert!(FollowedArtist::get(&pool, "u1", "a1")
            .await
            .unwrap()
            .is_none());
        assert!(ArtistRelease::releases_for_artist(&pool, "u1", "a1")
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            ArtistRelease::releases_for_artist(&pool, "u1", "a2")
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(!FollowedArtist::unfollow(&pool, "u1", "a1").await.unwrap());
    }

    #[tokio::test]
    async fn due_for_check_honours_null_and_age() {
        let pool = pool_with_user("u1").await;
        sqlx::query("INSERT INTO users (id, username, password_hash) VALUES ('u2', 'two', 'x')")
            .execute(&pool)
            .await
            .unwrap();
        FollowedArtist::follow(&pool, "u1", "never", "N")
            .await
            .unwrap();
        FollowedArtist::follow(&pool, "u1", "old", "O")
            .await
            .unwrap();
        FollowedArtist::follow(&pool, "u2", "fresh", "F")
            .await
            .unwrap();
        sqlx::query(
            "UPDATE followed_artists SET last_checked_at = datetime('now', '-13 hours')
             WHERE artist_mbid = 'old'",
        )
        .execute(&pool)
        .await
        .unwrap();
        FollowedArtist::record_check_success(&pool, "u2", "fresh")
            .await
            .unwrap();

        let due = FollowedArtist::due_for_check(&pool, 12).await.unwrap();
        let ids: Vec<&str> = due.iter().map(|f| f.artist_mbid.as_str()).collect();
        assert_eq!(ids, vec!["never", "old"]);

        let due = FollowedArtist::due_for_check(&pool, 14).await.unwrap();
        let ids: Vec<&str> = due.iter().map(|f| f.artist_mbid.as_str()).collect();
        assert_eq!(ids, vec!["never"]);
    }

    #[tokio::test]
    async fn a_failed_first_check_stays_unchecked() {
        let pool = pool_with_user("u1").await;
        FollowedArtist::follow(&pool, "u1", "a1", "A")
            .await
            .unwrap();
        FollowedArtist::record_check_failure(&pool, "u1", "a1", "boom")
            .await
            .unwrap();
        let f = FollowedArtist::get(&pool, "u1", "a1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(f.last_error.as_deref(), Some("boom"));
        assert!(f.last_checked_at.is_none());

        FollowedArtist::record_check_success(&pool, "u1", "a1")
            .await
            .unwrap();
        let f = FollowedArtist::get(&pool, "u1", "a1")
            .await
            .unwrap()
            .unwrap();
        assert!(f.last_error.is_none());
        assert!(f.last_checked_at.is_some());

        FollowedArtist::record_check_failure(&pool, "u1", "a1", "again")
            .await
            .unwrap();
        let f = FollowedArtist::get(&pool, "u1", "a1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(f.last_error.as_deref(), Some("again"));
        assert!(f.last_checked_at.is_some());
    }
}
