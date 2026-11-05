use serde::{Deserialize, Serialize};

use crate::metadata::{ArtistSummary, ReleaseKind};

/// How a followed artist is monitored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FollowSettings {
    pub include_singles: bool,
    pub auto_download: bool,
    pub folder_id: Option<String>,
}

/// One release group on an artist's page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtistReleaseView {
    pub release_group_mbid: String,
    pub title: String,
    pub kind: ReleaseKind,
    pub first_release_date: Option<String>,
    /// The stored status, or `None` when the release is not tracked.
    pub status: Option<String>,
    pub status_detail: Option<String>,
    pub in_library: bool,
}

/// An artist with their releases and the user's follow, if any.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtistPage {
    pub artist: ArtistSummary,
    pub follow: Option<FollowSettings>,
    pub last_checked_at: Option<String>,
    pub last_error: Option<String>,
    pub releases: Vec<ArtistReleaseView>,
}

/// A followed artist in the user's list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FollowedArtistView {
    pub artist_mbid: String,
    pub name: String,
    pub settings: FollowSettings,
    pub last_checked_at: Option<String>,
    pub last_error: Option<String>,
    pub new_count: u32,
    pub upcoming_count: u32,
}

/// A release that needs the user's attention or is on its way.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewReleaseView {
    pub artist_mbid: String,
    pub artist_name: String,
    pub release: ArtistReleaseView,
}

/// The outcome of downloading a followed release.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReleaseDownloadResult {
    pub status: String,
    pub detail: Option<String>,
}

/// What a check of a followed artist did.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CheckResult {
    pub already_running: bool,
    pub added_new: u32,
    pub added_upcoming: u32,
    pub became_new: u32,
    pub queued: u32,
    pub manual: u32,
    pub failed: u32,
}
