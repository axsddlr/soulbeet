//! What a check of a followed artist changes, worked out without I/O.

use std::collections::HashMap;

use shared::metadata::{ReleaseGroupSummary, ReleaseKind};

use crate::models::artist_release::ReleaseStatus;

/// What a check needs to know of a release group already stored.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredRelease {
    pub status: ReleaseStatus,
    pub first_release_date: Option<String>,
}

/// One change a check makes.
#[derive(Debug, Clone, PartialEq)]
pub enum ReleaseChange {
    /// A release group not stored before.
    Insert {
        release_group_mbid: String,
        title: String,
        kind: ReleaseKind,
        first_release_date: Option<String>,
        status: ReleaseStatus,
    },
    /// A stored release group moves to `to`, taking MusicBrainz's current
    /// `first_release_date`.
    Transition {
        release_group_mbid: String,
        to: ReleaseStatus,
        first_release_date: Option<String>,
    },
}

/// The earliest day a MusicBrainz date can mean, as `YYYY-MM-DD`: "2027"
/// is 2027-01-01, "2027-05" is 2027-05-01. `None` for no date.
fn earliest_day(date: Option<&str>) -> Option<String> {
    let date = date?.trim();
    match date.len() {
        0 => None,
        4 => Some(format!("{date}-01-01")),
        7 => Some(format!("{date}-01")),
        _ => Some(date.to_string()),
    }
}

/// Whether a release dated `date` is still ahead of `today` (YYYY-MM-DD).
/// An undated release counts as out.
fn is_upcoming(date: Option<&str>, today: &str) -> bool {
    earliest_day(date).is_some_and(|day| day.as_str() > today)
}

/// The changes a check makes, given the `stored` releases (by release
/// group id) and the release groups just `fetched` from MusicBrainz.
///
/// Only plain Albums, EPs and Singles count. An unseen single is Known
/// unless `include_singles`. Any other unseen group is Upcoming while its
/// first release date is after `today`, even on a `baseline` check, so an
/// album announced before the follow still turns up when it comes out.
/// Otherwise it is Known on a baseline check, else New. A stored Upcoming group whose
/// date has come becomes New. Nothing else stored is touched, including
/// groups MusicBrainz no longer lists.
pub fn plan_check(
    stored: &HashMap<String, StoredRelease>,
    fetched: &[ReleaseGroupSummary],
    include_singles: bool,
    baseline: bool,
    today: &str,
) -> Vec<ReleaseChange> {
    let mut changes = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for group in fetched {
        let Some(kind) = group.kind() else {
            continue;
        };
        if !seen.insert(group.mbid.as_str()) {
            continue;
        }
        let date = group.first_release_date.as_deref();
        match stored.get(&group.mbid) {
            None => {
                let status = if kind == ReleaseKind::Single && !include_singles {
                    ReleaseStatus::Known
                } else if is_upcoming(date, today) {
                    ReleaseStatus::Upcoming
                } else if baseline {
                    ReleaseStatus::Known
                } else {
                    ReleaseStatus::New
                };
                changes.push(ReleaseChange::Insert {
                    release_group_mbid: group.mbid.clone(),
                    title: group.title.clone(),
                    kind,
                    first_release_date: group.first_release_date.clone(),
                    status,
                });
            }
            Some(row) if row.status == ReleaseStatus::Upcoming => {
                // A date MusicBrainz dropped keeps the stored one.
                let date = group
                    .first_release_date
                    .clone()
                    .or_else(|| row.first_release_date.clone());
                if !is_upcoming(date.as_deref(), today) {
                    changes.push(ReleaseChange::Transition {
                        release_group_mbid: group.mbid.clone(),
                        to: ReleaseStatus::New,
                        first_release_date: date,
                    });
                }
            }
            Some(_) => {}
        }
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;

    const TODAY: &str = "2026-11-03";

    fn rg(
        mbid: &str,
        primary: &str,
        secondary: &[&str],
        date: Option<&str>,
    ) -> ReleaseGroupSummary {
        ReleaseGroupSummary {
            mbid: mbid.to_string(),
            title: format!("Title {mbid}"),
            primary_type: Some(primary.to_string()),
            secondary_types: secondary.iter().map(|s| s.to_string()).collect(),
            first_release_date: date.map(str::to_string),
        }
    }

    fn stored(entries: &[(&str, ReleaseStatus, Option<&str>)]) -> HashMap<String, StoredRelease> {
        entries
            .iter()
            .map(|(mbid, status, date)| {
                (
                    mbid.to_string(),
                    StoredRelease {
                        status: *status,
                        first_release_date: date.map(str::to_string),
                    },
                )
            })
            .collect()
    }

    /// The inserted status of each group, in order.
    fn inserted(changes: &[ReleaseChange]) -> Vec<(String, ReleaseStatus)> {
        changes
            .iter()
            .filter_map(|c| match c {
                ReleaseChange::Insert {
                    release_group_mbid,
                    status,
                    ..
                } => Some((release_group_mbid.clone(), *status)),
                _ => None,
            })
            .collect()
    }

    fn ids(pairs: &[(&str, ReleaseStatus)]) -> Vec<(String, ReleaseStatus)> {
        pairs.iter().map(|(m, s)| (m.to_string(), *s)).collect()
    }

    #[test]
    fn baseline_marks_released_groups_known_and_announced_ones_upcoming() {
        let fetched = [
            rg("a", "Album", &[], Some("2020-01-01")),
            rg("b", "EP", &[], Some("2099-01-01")),
            rg("c", "Single", &[], None),
        ];
        let changes = plan_check(&HashMap::new(), &fetched, true, true, TODAY);
        assert_eq!(
            inserted(&changes),
            ids(&[
                ("a", ReleaseStatus::Known),
                ("b", ReleaseStatus::Upcoming),
                ("c", ReleaseStatus::Known),
            ])
        );
    }

    #[test]
    fn insert_carries_the_group_details() {
        let changes = plan_check(
            &HashMap::new(),
            &[rg("a", "EP", &[], Some("2020"))],
            false,
            false,
            TODAY,
        );
        assert_eq!(
            changes,
            vec![ReleaseChange::Insert {
                release_group_mbid: "a".into(),
                title: "Title a".into(),
                kind: ReleaseKind::Ep,
                first_release_date: Some("2020".into()),
                status: ReleaseStatus::New,
            }]
        );
    }

    #[test]
    fn secondary_typed_and_unknown_groups_are_ignored() {
        let fetched = [
            rg("live", "Album", &["Live"], Some("2020-01-01")),
            rg("comp", "Album", &["Compilation"], None),
            rg("other", "Other", &[], None),
            rg("broadcast", "Broadcast", &[], None),
            rg("ok", "Album", &[], Some("2020-01-01")),
        ];
        let changes = plan_check(&HashMap::new(), &fetched, true, false, TODAY);
        assert_eq!(inserted(&changes), ids(&[("ok", ReleaseStatus::New)]));
    }

    #[test]
    fn singles_follow_the_include_singles_setting() {
        let fetched = [rg("s", "Single", &[], Some("2020-01-01"))];
        let off = plan_check(&HashMap::new(), &fetched, false, false, TODAY);
        assert_eq!(inserted(&off), ids(&[("s", ReleaseStatus::Known)]));
        let on = plan_check(&HashMap::new(), &fetched, true, false, TODAY);
        assert_eq!(inserted(&on), ids(&[("s", ReleaseStatus::New)]));
    }

    #[test]
    fn a_future_date_is_upcoming_and_today_or_none_is_new() {
        let fetched = [
            rg("future", "Album", &[], Some("2026-11-04")),
            rg("today", "Album", &[], Some(TODAY)),
            rg("past", "Album", &[], Some("2001-02-03")),
            rg("undated", "Album", &[], None),
            rg("blank", "Album", &[], Some("")),
        ];
        let changes = plan_check(&HashMap::new(), &fetched, false, false, TODAY);
        assert_eq!(
            inserted(&changes),
            ids(&[
                ("future", ReleaseStatus::Upcoming),
                ("today", ReleaseStatus::New),
                ("past", ReleaseStatus::New),
                ("undated", ReleaseStatus::New),
                ("blank", ReleaseStatus::New),
            ])
        );
    }

    #[test]
    fn partial_dates_count_as_their_earliest_day() {
        let fetched = [
            rg("this-year", "Album", &[], Some("2026")),
            rg("next-year", "Album", &[], Some("2027")),
            rg("this-month", "Album", &[], Some("2026-11")),
            rg("next-month", "Album", &[], Some("2026-12")),
            rg("last-month", "Album", &[], Some("2026-10")),
        ];
        let changes = plan_check(&HashMap::new(), &fetched, false, false, TODAY);
        assert_eq!(
            inserted(&changes),
            ids(&[
                ("this-year", ReleaseStatus::New),
                ("next-year", ReleaseStatus::Upcoming),
                ("this-month", ReleaseStatus::New),
                ("next-month", ReleaseStatus::Upcoming),
                ("last-month", ReleaseStatus::New),
            ])
        );
    }

    #[test]
    fn upcoming_becomes_new_once_its_date_passes() {
        let have = stored(&[
            ("due", ReleaseStatus::Upcoming, Some("2026-11")),
            ("later", ReleaseStatus::Upcoming, Some("2027-01-01")),
        ]);
        let fetched = [
            rg("due", "Album", &[], Some("2026-11-02")),
            rg("later", "Album", &[], Some("2027-01-01")),
        ];
        let changes = plan_check(&have, &fetched, false, false, TODAY);
        assert_eq!(
            changes,
            vec![ReleaseChange::Transition {
                release_group_mbid: "due".into(),
                to: ReleaseStatus::New,
                first_release_date: Some("2026-11-02".into()),
            }]
        );
    }

    #[test]
    fn upcoming_moves_on_even_in_a_baseline_check() {
        let have = stored(&[("due", ReleaseStatus::Upcoming, Some("2026-01-01"))]);
        let fetched = [rg("due", "Album", &[], Some("2026-01-01"))];
        let changes = plan_check(&have, &fetched, false, true, TODAY);
        assert_eq!(changes.len(), 1);
    }

    #[test]
    fn other_stored_statuses_are_untouched() {
        let statuses = [
            ReleaseStatus::Known,
            ReleaseStatus::New,
            ReleaseStatus::Queued,
            ReleaseStatus::Manual,
            ReleaseStatus::Failed,
            ReleaseStatus::Dismissed,
        ];
        let names: Vec<String> = (0..statuses.len()).map(|i| format!("rg{i}")).collect();
        let entries: Vec<(&str, ReleaseStatus, Option<&str>)> = names
            .iter()
            .zip(statuses)
            .map(|(n, s)| (n.as_str(), s, Some("2099-01-01")))
            .collect();
        let have = stored(&entries);
        // Dates moved both ways and singles excluded: still nothing.
        let fetched: Vec<_> = names
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let date = if i % 2 == 0 {
                    "2001-01-01"
                } else {
                    "2099-01-01"
                };
                rg(n, "Single", &[], Some(date))
            })
            .collect();
        assert!(plan_check(&have, &fetched, false, false, TODAY).is_empty());
        assert!(plan_check(&have, &fetched, true, true, TODAY).is_empty());
    }

    #[test]
    fn upcoming_keeps_its_stored_date_when_musicbrainz_drops_it() {
        let have = stored(&[
            ("ahead", ReleaseStatus::Upcoming, Some("2099-01-01")),
            ("passed", ReleaseStatus::Upcoming, Some("2001-01-01")),
        ]);
        let fetched = [
            rg("ahead", "Album", &[], None),
            rg("passed", "Album", &[], None),
        ];
        let changes = plan_check(&have, &fetched, false, false, TODAY);
        assert_eq!(
            changes,
            vec![ReleaseChange::Transition {
                release_group_mbid: "passed".into(),
                to: ReleaseStatus::New,
                first_release_date: Some("2001-01-01".into()),
            }]
        );
    }

    #[test]
    fn groups_gone_from_musicbrainz_are_left_alone() {
        let have = stored(&[("gone", ReleaseStatus::Upcoming, Some("2001-01-01"))]);
        assert!(plan_check(&have, &[], false, false, TODAY).is_empty());
    }

    #[test]
    fn a_group_listed_twice_is_inserted_once() {
        let fetched = [
            rg("a", "Album", &[], Some("2020-01-01")),
            rg("a", "Album", &[], Some("2020-01-01")),
        ];
        let changes = plan_check(&HashMap::new(), &fetched, false, false, TODAY);
        assert_eq!(changes.len(), 1);
    }
}
