//! Pieces shared by the artist pages: status labels, discography grouping,
//! relative times, release-group covers, chips and skeletons. The pages
//! themselves live in the web crate, which owns the router.

use chrono::{DateTime, NaiveDateTime, Utc};
use dioxus::prelude::*;
use shared::artists::{ArtistReleaseView, CheckResult, ReleaseDownloadResult};
use shared::metadata::{ArtistSummary, ReleaseKind};

/// The cover of a release group, from the Cover Art Archive.
pub fn release_group_cover_url(rg_mbid: &str) -> String {
    format!("https://coverartarchive.org/release-group/{rg_mbid}/front-250")
}

/// How a release kind reads on a card.
pub fn kind_label(kind: ReleaseKind) -> &'static str {
    match kind {
        ReleaseKind::Album => "Album",
        ReleaseKind::Ep => "EP",
        ReleaseKind::Single => "Single",
    }
}

/// The heading of a discography section.
pub fn kind_section_title(kind: ReleaseKind) -> &'static str {
    match kind {
        ReleaseKind::Album => "Albums",
        ReleaseKind::Ep => "EPs",
        ReleaseKind::Single => "Singles",
    }
}

/// The year of a release date, when it starts with one.
pub fn release_year(date: Option<&str>) -> Option<String> {
    let y = date?.get(..4)?;
    y.chars().all(|c| c.is_ascii_digit()).then(|| y.to_string())
}

/// The releases split into Albums, EPs and Singles, newest first in each.
/// Empty sections are left out.
pub fn group_by_kind(releases: &[ArtistReleaseView]) -> Vec<(ReleaseKind, Vec<ArtistReleaseView>)> {
    [ReleaseKind::Album, ReleaseKind::Ep, ReleaseKind::Single]
        .into_iter()
        .filter_map(|kind| {
            let mut group: Vec<ArtistReleaseView> = releases
                .iter()
                .filter(|r| r.kind == kind)
                .cloned()
                .collect();
            if group.is_empty() {
                return None;
            }
            // Undated releases sort last; ties keep MusicBrainz's order.
            group.sort_by(
                |a, b| match (&a.first_release_date, &b.first_release_date) {
                    (Some(x), Some(y)) => y.cmp(x),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                },
            );
            Some((kind, group))
        })
        .collect()
}

/// The chip shown for a tracked release: its label and colours. Releases
/// already out when the artist was followed ("known") and untracked ones get
/// none.
pub fn status_chip(status: &str, detail: Option<&str>) -> Option<(String, &'static str)> {
    let chip = match status {
        "new" => ("New".to_string(), "bg-beet-accent text-black"),
        "upcoming" => ("Upcoming".to_string(), "bg-sky-500/20 text-sky-300"),
        "queued" => ("Queued".to_string(), "bg-white/10 text-white"),
        "manual" => (
            "Needs manual pick".to_string(),
            "bg-amber-500/20 text-amber-300",
        ),
        "failed" => (
            match detail.map(str::trim).filter(|d| !d.is_empty()) {
                Some(d) => format!("Failed: {d}"),
                None => "Failed".to_string(),
            },
            "bg-red-500/20 text-red-300",
        ),
        "dismissed" => ("Dismissed".to_string(), "bg-white/5 text-muted"),
        _ => return None,
    };
    Some(chip)
}

/// Whether a release in this status can be downloaded.
pub fn can_download(status: Option<&str>) -> bool {
    matches!(status, Some("new" | "manual" | "failed" | "known"))
}

/// Whether a release in this status can be dismissed from the new list.
pub fn can_dismiss(status: Option<&str>) -> bool {
    matches!(status, Some("new" | "upcoming" | "manual" | "failed"))
}

/// Releases waiting on the user, for the nav badge.
pub fn new_count<'a>(statuses: impl IntoIterator<Item = &'a str>) -> usize {
    statuses.into_iter().filter(|s| *s == "new").count()
}

/// The line under an artist's name: disambiguation, kind and country.
pub fn artist_meta(artist: &ArtistSummary) -> String {
    [
        artist.disambiguation.as_deref(),
        artist.kind.as_deref(),
        artist.country.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join(" · ")
}

/// A short "3m ago" for a stored timestamp (SQLite `datetime('now')` or
/// RFC 3339). Unparseable input comes back as given.
pub fn relative_time(ts: &str, now: DateTime<Utc>) -> String {
    let parsed = DateTime::parse_from_rfc3339(ts)
        .map(|t| t.with_timezone(&Utc))
        .or_else(|_| NaiveDateTime::parse_from_str(ts, "%Y-%m-%d %H:%M:%S").map(|n| n.and_utc()));
    let Ok(parsed) = parsed else {
        return ts.to_string();
    };
    let delta = now.signed_duration_since(parsed);
    let mins = delta.num_minutes();
    if mins < 1 {
        return "just now".to_string();
    }
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = delta.num_hours();
    if hours < 24 {
        return format!("{hours}h ago");
    }
    let days = delta.num_days();
    if days < 365 {
        return format!("{days}d ago");
    }
    format!("{}y ago", days / 365)
}

/// The kind and date of a release as one line, e.g. "Album · 2025-01-01".
pub fn kind_and_date(kind: ReleaseKind, date: Option<&str>) -> String {
    match date.filter(|d| !d.is_empty()) {
        Some(d) => format!("{} · {d}", kind_label(kind)),
        None => kind_label(kind).to_string(),
    }
}

/// What a check did, in one sentence.
pub fn check_summary(result: &CheckResult) -> String {
    if result.already_running {
        return "A check is already running.".to_string();
    }
    let parts: Vec<String> = [
        (result.added_new + result.became_new, "new"),
        (result.added_upcoming, "upcoming"),
        (result.queued, "queued"),
        (result.manual, "need a manual pick"),
        (result.failed, "failed"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, label)| format!("{n} {label}"))
    .collect();
    if parts.is_empty() {
        "Checked. Nothing new.".to_string()
    } else {
        format!("Checked: {}.", parts.join(", "))
    }
}

/// The outcome of a download, for the line under its release.
pub fn download_outcome(result: &ReleaseDownloadResult) -> (String, bool) {
    let detail = result
        .detail
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty());
    match result.status.as_str() {
        "queued" => ("Queued. Progress shows in Downloads.".to_string(), false),
        "manual" => (
            "No close Soulseek match. Search for it manually.".to_string(),
            true,
        ),
        "failed" => (
            match detail {
                Some(d) => format!("Download failed: {d}"),
                None => "Download failed.".to_string(),
            },
            true,
        ),
        other => (
            match detail {
                Some(d) => format!("{other}: {d}"),
                None => other.to_string(),
            },
            false,
        ),
    }
}

/// A release group's cover, or a note icon when the archive has none.
#[component]
pub fn ReleaseCover(rg_mbid: String, title: String, #[props(default)] class: String) -> Element {
    let mut has_error = use_signal(|| false);
    let size = if class.is_empty() {
        "w-12 h-12 rounded".to_string()
    } else {
        class
    };
    let url = release_group_cover_url(&rg_mbid);

    rsx! {
        div { class: "{size} shrink-0 bg-beet-dim flex items-center justify-center overflow-hidden",
            if !has_error() {
                img {
                    src: "{url}",
                    alt: "Cover for {title}",
                    loading: "lazy",
                    class: "w-full h-full object-cover",
                    onerror: move |_| has_error.set(true),
                }
            } else {
                svg {
                    class: "w-1/3 h-1/3 max-w-12 max-h-12 text-white/30",
                    fill: "none",
                    view_box: "0 0 24 24",
                    stroke_width: "1.5",
                    stroke: "currentColor",
                    "aria-hidden": "true",
                    path {
                        stroke_linecap: "round",
                        stroke_linejoin: "round",
                        d: "M9 9l10.5-3m0 6.553v3.75a2.25 2.25 0 01-1.632 2.163l-1.32.377a1.803 1.803 0 11-.99-3.467l2.31-.66a2.25 2.25 0 001.632-2.163zm0 0V2.25L9 5.25v10.303m0 0v3.75a2.25 2.25 0 01-1.632 2.163l-1.32.377a1.803 1.803 0 01-.99-3.467l2.31-.66A2.25 2.25 0 009 15.553z",
                    }
                }
            }
        }
    }
}

/// The status chip of a tracked release; nothing for untracked or known ones.
#[component]
pub fn StatusChip(status: Option<String>, detail: Option<String>) -> Element {
    let Some((label, colors)) = status
        .as_deref()
        .and_then(|s| status_chip(s, detail.as_deref()))
    else {
        return rsx! {};
    };
    rsx! {
        span {
            class: "inline-block max-w-full truncate align-middle text-[11px] font-bold px-2 py-0.5 rounded-full {colors}",
            title: "{label}",
            "data-testid": "status-chip",
            "data-status": status.unwrap_or_default(),
            "{label}"
        }
    }
}

/// A small grey chip, e.g. "In library" or "Auto-download on".
#[component]
pub fn InfoChip(label: String, #[props(default)] accent: bool) -> Element {
    let colors = if accent {
        "bg-beet-accent/15 text-beet-leaf"
    } else {
        "bg-white/10 text-muted"
    };
    rsx! {
        span { class: "inline-block text-[11px] font-bold px-2 py-0.5 rounded-full whitespace-nowrap {colors}",
            "{label}"
        }
    }
}

/// Placeholder rows while a list loads, sized like the real ones.
#[component]
pub fn RowSkeleton(#[props(default = 4)] rows: usize) -> Element {
    rsx! {
        div { class: "flex flex-col gap-2", "aria-busy": "true",
            for i in 0..rows {
                div { key: "{i}", class: "flex items-center gap-3 p-2 rounded-md",
                    div { class: "w-12 h-12 rounded bg-white/5 animate-pulse shrink-0" }
                    div { class: "flex-1 min-w-0 space-y-2",
                        div { class: "h-4 w-1/2 max-w-64 rounded bg-white/5 animate-pulse" }
                        div { class: "h-3 w-1/3 max-w-40 rounded bg-white/5 animate-pulse" }
                    }
                }
            }
        }
    }
}

/// Placeholder release cards while a discography loads.
#[component]
pub fn CardGridSkeleton(#[props(default = 6)] cards: usize) -> Element {
    rsx! {
        div { class: "grid grid-cols-2 sm:grid-cols-3 md:grid-cols-[repeat(auto-fill,minmax(180px,1fr))] gap-4",
            "aria-busy": "true",
            for i in 0..cards {
                div { key: "{i}", class: "p-3 md:p-4 rounded-md bg-beet-panel",
                    div { class: "w-full aspect-square rounded-md bg-white/5 animate-pulse mb-3 md:mb-4" }
                    div { class: "h-4 w-3/4 rounded bg-white/5 animate-pulse" }
                    div { class: "h-3 w-1/3 rounded bg-white/5 animate-pulse mt-2" }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn release(title: &str, kind: ReleaseKind, date: Option<&str>) -> ArtistReleaseView {
        ArtistReleaseView {
            release_group_mbid: format!("rg-{title}"),
            title: title.to_string(),
            kind,
            first_release_date: date.map(str::to_string),
            status: None,
            status_detail: None,
            in_library: false,
        }
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 3, 12, 0, 0).unwrap()
    }

    #[test]
    fn groups_by_kind_in_section_order_newest_first() {
        let releases = vec![
            release("single", ReleaseKind::Single, Some("2020-02-07")),
            release("old", ReleaseKind::Album, Some("2015-03-06")),
            release("ep", ReleaseKind::Ep, Some("2019-06-21")),
            release("undated", ReleaseKind::Album, None),
            release("future", ReleaseKind::Album, Some("2099-01-01")),
        ];
        let groups = group_by_kind(&releases);
        let kinds: Vec<_> = groups.iter().map(|(k, _)| *k).collect();
        assert_eq!(
            kinds,
            vec![ReleaseKind::Album, ReleaseKind::Ep, ReleaseKind::Single]
        );
        let albums: Vec<_> = groups[0].1.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(albums, vec!["future", "old", "undated"]);
    }

    #[test]
    fn leaves_empty_sections_out() {
        let groups = group_by_kind(&[release("a", ReleaseKind::Album, None)]);
        assert_eq!(groups.len(), 1);
        assert_eq!(kind_section_title(groups[0].0), "Albums");
        assert!(group_by_kind(&[]).is_empty());
    }

    #[test]
    fn maps_statuses_to_chips() {
        assert_eq!(status_chip("new", None).unwrap().0, "New");
        assert_eq!(status_chip("upcoming", None).unwrap().0, "Upcoming");
        assert_eq!(status_chip("queued", None).unwrap().0, "Queued");
        assert_eq!(status_chip("manual", None).unwrap().0, "Needs manual pick");
        assert_eq!(
            status_chip("failed", Some("no peers")).unwrap().0,
            "Failed: no peers"
        );
        assert_eq!(status_chip("failed", Some("  ")).unwrap().0, "Failed");
        assert!(status_chip("known", None).is_none());
        assert!(status_chip("anything", None).is_none());
    }

    #[test]
    fn download_and_dismiss_follow_the_status() {
        for s in ["new", "manual", "failed", "known"] {
            assert!(can_download(Some(s)), "{s}");
        }
        for s in ["upcoming", "queued", "dismissed"] {
            assert!(!can_download(Some(s)), "{s}");
        }
        assert!(!can_download(None));
        for s in ["new", "upcoming", "manual", "failed"] {
            assert!(can_dismiss(Some(s)), "{s}");
        }
        for s in ["known", "queued", "dismissed"] {
            assert!(!can_dismiss(Some(s)), "{s}");
        }
        assert!(!can_dismiss(None));
    }

    #[test]
    fn counts_only_new_releases() {
        assert_eq!(new_count(["new", "upcoming", "new", "failed"]), 2);
        assert_eq!(new_count([]), 0);
    }

    #[test]
    fn formats_relative_times() {
        assert_eq!(relative_time("2026-11-03 11:59:30", now()), "just now");
        assert_eq!(relative_time("2026-11-03 11:55:00", now()), "5m ago");
        assert_eq!(relative_time("2026-11-03 09:00:00", now()), "3h ago");
        assert_eq!(relative_time("2026-10-31T12:00:00Z", now()), "3d ago");
        assert_eq!(relative_time("2024-11-01 12:00:00", now()), "2y ago");
        assert_eq!(relative_time("garbage", now()), "garbage");
    }

    #[test]
    fn reads_years_and_kind_lines() {
        assert_eq!(release_year(Some("2018-09-14")).as_deref(), Some("2018"));
        assert_eq!(release_year(Some("19xx")), None);
        assert_eq!(release_year(None), None);
        assert_eq!(
            kind_and_date(ReleaseKind::Ep, Some("2019-06-21")),
            "EP · 2019-06-21"
        );
        assert_eq!(kind_and_date(ReleaseKind::Single, None), "Single");
    }

    #[test]
    fn joins_artist_meta() {
        let artist = ArtistSummary {
            mbid: "m".into(),
            name: "n".into(),
            disambiguation: Some("UK rock band".into()),
            country: Some("GB".into()),
            kind: Some("Group".into()),
        };
        assert_eq!(artist_meta(&artist), "UK rock band · Group · GB");
        let bare = ArtistSummary {
            disambiguation: Some(" ".into()),
            country: None,
            kind: None,
            ..artist
        };
        assert_eq!(artist_meta(&bare), "");
    }

    #[test]
    fn summarises_checks() {
        assert_eq!(
            check_summary(&CheckResult::default()),
            "Checked. Nothing new."
        );
        let result = CheckResult {
            added_new: 1,
            became_new: 1,
            added_upcoming: 2,
            ..Default::default()
        };
        assert_eq!(check_summary(&result), "Checked: 2 new, 2 upcoming.");
        let running = CheckResult {
            already_running: true,
            ..Default::default()
        };
        assert_eq!(check_summary(&running), "A check is already running.");
    }

    #[test]
    fn describes_download_outcomes() {
        let outcome = |status: &str, detail: Option<&str>| {
            download_outcome(&ReleaseDownloadResult {
                status: status.into(),
                detail: detail.map(str::to_string),
            })
        };
        assert!(!outcome("queued", None).1);
        assert!(outcome("manual", None).1);
        assert_eq!(
            outcome("failed", Some("no peers")).0,
            "Download failed: no peers"
        );
        assert_eq!(outcome("failed", None).0, "Download failed.");
    }

    #[test]
    fn builds_release_group_cover_urls() {
        assert_eq!(
            release_group_cover_url("abc"),
            "https://coverartarchive.org/release-group/abc/front-250"
        );
    }
}
