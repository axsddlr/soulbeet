#[cfg(feature = "server")]
use super::folder::Folder;
#[cfg(feature = "server")]
use crate::db::DB;
#[cfg(feature = "server")]
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use serde::{Deserialize, Serialize};
#[cfg(feature = "server")]
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "server", derive(sqlx::FromRow))]
pub struct User {
    pub id: String,
    pub username: String,
    #[serde(skip)]
    pub password_hash: String,
    pub navidrome_token: Option<String>,
    pub navidrome_status: String,
}

#[cfg(feature = "server")]
impl User {
    pub async fn create(username: &str, password: &str) -> Result<User, String> {
        create_in(&DB, username, password).await
    }

    pub async fn verify(username: &str, password: &str) -> Result<User, String> {
        let user = find_by_username(&DB, username)
            .await?
            .ok_or("User not found")?;

        let parsed_hash = PasswordHash::new(&user.password_hash).map_err(|e| e.to_string())?;
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed_hash)
            .map_err(|_| "Invalid password")?;

        Ok(user)
    }

    pub async fn get_folders(&self) -> Result<Vec<Folder>, String> {
        sqlx::query_as::<_, Folder>("SELECT * FROM folders WHERE user_id = ?")
            .bind(&self.id)
            .fetch_all(&*DB)
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn get_all() -> Result<Vec<User>, String> {
        sqlx::query_as::<_, User>("SELECT * FROM users")
            .fetch_all(&*DB)
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn get_by_id(id: &str) -> Result<User, String> {
        let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = ?")
            .bind(id)
            .fetch_optional(&*DB)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("User not found")?;

        Ok(user)
    }

    pub async fn update_password(id: &str, password: &str) -> Result<(), String> {
        let salt = SaltString::generate(&mut OsRng);
        let argon2 = Argon2::default();
        let password_hash = argon2
            .hash_password(password.as_bytes(), &salt)
            .map_err(|e| e.to_string())?
            .to_string();

        sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?")
            .bind(password_hash)
            .bind(id)
            .execute(&*DB)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub async fn update_username(id: &str, new_username: &str) -> Result<(), String> {
        update_username_in(&DB, id, new_username).await
    }

    pub async fn delete(id: &str) -> Result<(), String> {
        sqlx::query("DELETE FROM users WHERE id = ?")
            .bind(id)
            .execute(&*DB)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub async fn get_by_username(username: &str) -> Result<Option<User>, String> {
        find_by_username(&DB, username).await
    }

    pub async fn update_navidrome_token(
        id: &str,
        token: Option<&str>,
        status: &str,
    ) -> Result<(), String> {
        sqlx::query("UPDATE users SET navidrome_token = ?, navidrome_status = ? WHERE id = ?")
            .bind(token)
            .bind(status)
            .bind(id)
            .execute(&*DB)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub async fn get_connected_users() -> Result<Vec<User>, String> {
        sqlx::query_as::<_, User>("SELECT * FROM users WHERE navidrome_status IN (?, ?)")
            .bind(shared::system::NavidromeStatus::Connected.as_str())
            .bind(shared::system::NavidromeStatus::MissingReportRealPath.as_str())
            .fetch_all(&*DB)
            .await
            .map_err(|e| e.to_string())
    }
}

/// Case-insensitive lookup (Navidrome usernames compare case-insensitively).
/// If legacy case-variant duplicates exist, prefer the exact-case match, then
/// the oldest row.
#[cfg(feature = "server")]
const FIND_BY_USERNAME: &str = "SELECT * FROM users WHERE username = ? ORDER BY rowid LIMIT 1";

#[cfg(feature = "server")]
async fn find_by_username(
    pool: &sqlx::SqlitePool,
    username: &str,
) -> Result<Option<User>, String> {
    sqlx::query_as::<_, User>(FIND_BY_USERNAME)
        .bind(username)
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())
}

#[cfg(feature = "server")]
async fn create_in(
    pool: &sqlx::SqlitePool,
    username: &str,
    password: &str,
) -> Result<User, String> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let password_hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| e.to_string())?
        .to_string();

    let id = Uuid::new_v4().to_string();

    sqlx::query_as::<_, User>(
        "INSERT INTO users (id, username, password_hash, navidrome_status) VALUES (?, ?, ?, ?) RETURNING *"
    )
    .bind(&id)
    .bind(username)
    .bind(password_hash)
    .bind(shared::system::NavidromeStatus::Unknown.as_str())
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())
}

#[cfg(feature = "server")]
async fn update_username_in(
    pool: &sqlx::SqlitePool,
    id: &str,
    new_username: &str,
) -> Result<(), String> {
    sqlx::query("UPDATE users SET username = ? WHERE id = ?")
        .bind(new_username)
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(all(test, feature = "server"))]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn pool() -> sqlx::SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    async fn insert_raw(pool: &sqlx::SqlitePool, id: &str, name: &str) {
        sqlx::query("INSERT INTO users (id, username, password_hash) VALUES (?, ?, 'x')")
            .bind(id)
            .bind(name)
            .execute(pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn lookup_ignores_case() {
        let pool = pool().await;
        insert_raw(&pool, "u1", "anna").await;
        let found = find_by_username(&pool, "ANNA").await.unwrap().unwrap();
        assert_eq!(found.id, "u1");
        assert!(find_by_username(&pool, "bob").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn duplicates_prefer_exact_case_then_oldest() {
        let pool = pool().await;
        insert_raw(&pool, "old", "Anna").await;
        insert_raw(&pool, "new", "anna").await;
        let exact = find_by_username(&pool, "anna").await.unwrap().unwrap();
        assert_eq!(exact.id, "new");
        let other = find_by_username(&pool, "ANNA").await.unwrap().unwrap();
        assert_eq!(other.id, "old");
    }

    #[tokio::test]
    async fn create_rejects_case_variant_of_existing_user() {
        let pool = pool().await;
        create_in(&pool, "anna", "pw").await.unwrap();
        assert!(create_in(&pool, "ANNA", "pw").await.is_err());
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE username LIKE 'anna'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn rename_rejects_case_variant_of_another_user() {
        let pool = pool().await;
        insert_raw(&pool, "u1", "anna").await;
        insert_raw(&pool, "u2", "bob").await;
        assert!(update_username_in(&pool, "u2", "ANNA").await.is_err());
        // Changing only the case of one's own name is fine.
        update_username_in(&pool, "u1", "Anna").await.unwrap();
    }
}
