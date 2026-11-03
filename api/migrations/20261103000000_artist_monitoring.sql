-- Artists a user follows for new releases.
CREATE TABLE IF NOT EXISTS followed_artists (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    artist_mbid TEXT NOT NULL,
    name TEXT NOT NULL,
    include_singles BOOLEAN NOT NULL DEFAULT 0,
    auto_download BOOLEAN NOT NULL DEFAULT 0,
    folder_id TEXT DEFAULT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    last_checked_at TEXT DEFAULT NULL,
    last_error TEXT DEFAULT NULL,
    PRIMARY KEY (user_id, artist_mbid)
);

-- Release groups seen for a followed artist. `kind` is 'album', 'ep' or
-- 'single'; `status` is one of known, upcoming, new, queued, manual,
-- failed, dismissed.
CREATE TABLE IF NOT EXISTS artist_releases (
    user_id TEXT NOT NULL,
    artist_mbid TEXT NOT NULL,
    release_group_mbid TEXT NOT NULL,
    title TEXT NOT NULL,
    kind TEXT NOT NULL,
    first_release_date TEXT DEFAULT NULL,
    status TEXT NOT NULL,
    status_detail TEXT DEFAULT NULL,
    discovered_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (user_id, release_group_mbid),
    FOREIGN KEY (user_id, artist_mbid)
        REFERENCES followed_artists(user_id, artist_mbid) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_artist_releases_user_status
ON artist_releases(user_id, status);

CREATE INDEX IF NOT EXISTS idx_artist_releases_user_artist
ON artist_releases(user_id, artist_mbid);
