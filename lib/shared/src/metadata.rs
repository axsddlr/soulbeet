use serde::{Deserialize, Serialize};

/// Metadata provider identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[default]
    MusicBrainz,
    LastFm,
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Provider::MusicBrainz => write!(f, "musicbrainz"),
            Provider::LastFm => write!(f, "lastfm"),
        }
    }
}

impl std::str::FromStr for Provider {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "musicbrainz" => Ok(Provider::MusicBrainz),
            "lastfm" => Ok(Provider::LastFm),
            _ => Err(format!("Unknown provider: {}", s)),
        }
    }
}

/// Wrapper for search results that includes provider information.
#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResults {
    pub provider: Provider,
    pub results: Vec<SearchResult>,
}

/// Represents a search result which can be either a track or an album.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum SearchResult {
    Track(Track),
    Album(Album),
}

/// A track from a metadata provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    /// Provider-specific identifier for lookups.
    pub id: String,
    /// The title of the track.
    pub title: String,
    /// A formatted string of the artist(s).
    pub artist: String,
    /// Provider-specific identifier of the album the track belongs to.
    pub album_id: Option<String>,
    /// The title of the album the track belongs to.
    pub album_title: Option<String>,
    /// The release date of the album (YYYY-MM-DD).
    pub release_date: Option<String>,
    /// The duration of the track in a formatted MM:SS string.
    pub duration: Option<String>,
    /// The MusicBrainz recording ID, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mbid: Option<String>,
    /// The MusicBrainz release ID for the album, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_mbid: Option<String>,
}

/// An album from a metadata provider.
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct Album {
    /// Provider-specific identifier for lookups.
    pub id: String,
    /// The title of the album.
    pub title: String,
    /// A formatted string of the artist(s).
    pub artist: String,
    /// The release date of the album (YYYY-MM-DD).
    pub release_date: Option<String>,
    /// The MusicBrainz release ID, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mbid: Option<String>,
    /// URL to the album cover image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
}

/// An album with its full track listing.
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct AlbumWithTracks {
    pub album: Album,
    pub tracks: Vec<Track>,
}

/// A MusicBrainz artist, as shown in artist search results and monitoring.
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct ArtistSummary {
    pub mbid: String,
    pub name: String,
    pub disambiguation: Option<String>,
    pub country: Option<String>,
    /// MusicBrainz artist type, e.g. "Person" or "Group".
    pub kind: Option<String>,
}

/// The kinds of release group that artist monitoring surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReleaseKind {
    Album,
    Ep,
    Single,
}

/// A MusicBrainz release group listed under an artist.
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct ReleaseGroupSummary {
    pub mbid: String,
    pub title: String,
    pub primary_type: Option<String>,
    pub secondary_types: Vec<String>,
    pub first_release_date: Option<String>,
}

impl ReleaseGroupSummary {
    /// The monitorable kind of this release group. `Some` only for a plain
    /// Album/EP/Single: any secondary type (compilation, live, remix,
    /// soundtrack, DJ-mix, mixtape, demo, ...) excludes it.
    pub fn kind(&self) -> Option<ReleaseKind> {
        if !self.secondary_types.is_empty() {
            return None;
        }
        let primary = self.primary_type.as_deref()?;
        if primary.eq_ignore_ascii_case("album") {
            Some(ReleaseKind::Album)
        } else if primary.eq_ignore_ascii_case("ep") {
            Some(ReleaseKind::Ep)
        } else if primary.eq_ignore_ascii_case("single") {
            Some(ReleaseKind::Single)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod release_group_tests {
    use super::*;

    fn rg(primary: Option<&str>, secondary: &[&str]) -> ReleaseGroupSummary {
        ReleaseGroupSummary {
            mbid: "m".into(),
            title: "t".into(),
            primary_type: primary.map(Into::into),
            secondary_types: secondary.iter().map(|s| s.to_string()).collect(),
            first_release_date: None,
        }
    }

    #[test]
    fn kind_accepts_album_ep_single_case_insensitively() {
        assert_eq!(rg(Some("Album"), &[]).kind(), Some(ReleaseKind::Album));
        assert_eq!(rg(Some("album"), &[]).kind(), Some(ReleaseKind::Album));
        assert_eq!(rg(Some("EP"), &[]).kind(), Some(ReleaseKind::Ep));
        assert_eq!(rg(Some("ep"), &[]).kind(), Some(ReleaseKind::Ep));
        assert_eq!(rg(Some("SINGLE"), &[]).kind(), Some(ReleaseKind::Single));
    }

    #[test]
    fn kind_rejects_any_secondary_type() {
        for s in [
            "Compilation",
            "Live",
            "Remix",
            "Soundtrack",
            "DJ-mix",
            "Mixtape/Street",
            "Demo",
        ] {
            assert_eq!(rg(Some("Album"), &[s]).kind(), None, "{s}");
        }
    }

    #[test]
    fn kind_rejects_other_or_missing_primary() {
        assert_eq!(rg(Some("Broadcast"), &[]).kind(), None);
        assert_eq!(rg(Some("Other"), &[]).kind(), None);
        assert_eq!(rg(None, &[]).kind(), None);
    }
}
