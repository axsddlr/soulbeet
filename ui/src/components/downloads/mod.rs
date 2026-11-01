use std::cmp::Ordering;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use dioxus::logger::tracing::warn;
use dioxus::prelude::*;
use shared::download::{DownloadProgress, DownloadState, ImportChoice, KeptImport, MatchChoices};

mod activity_bar;
mod item;
mod picker;
pub use activity_bar::ActivityBar;
use api::CancelDownloadRequest;
use item::DownloadItem;

use crate::friendly_error;

#[derive(Props, Clone, PartialEq)]
pub struct DownloadsProps {
    pub is_open: Signal<bool>,
    pub downloads: Signal<HashMap<String, DownloadProgress>>,
}

#[component]
pub fn Downloads(mut props: DownloadsProps) -> Element {
    let groups = grouped_rows(&props.downloads.read());

    // Count specific states for the header summary
    let processing_count = groups
        .iter()
        .flat_map(|g| g.rows.iter())
        .filter(|f| {
            matches!(
                f.state,
                DownloadState::Queued
                    | DownloadState::InProgress
                    | DownloadState::ImportQueued(_)
                    | DownloadState::Importing
                    | DownloadState::Completed // Still needs to be imported
            )
        })
        .count();

    let errored_count = groups
        .iter()
        .flat_map(|g| g.rows.iter())
        .filter(|f| matches!(f.state, DownloadState::Failed(_) | DownloadState::Cancelled))
        .count();

    let clear_finished = move |_| {
        let mut cleared = Vec::new();
        props.downloads.write().retain(|item, file| {
            let keep = matches!(
                file.state,
                DownloadState::Queued
                    | DownloadState::InProgress
                    | DownloadState::ImportQueued(_)
                    | DownloadState::Importing
                    | DownloadState::Completed // Downloads that are completed but not yet imported
            );
            if !keep {
                cleared.push(item.clone());
            }
            keep
        });
        // The server keeps a retry for each row that did not import; drop those too.
        if !cleared.is_empty() {
            spawn(async move {
                if let Err(e) = api::forget_import_retries(cleared).await {
                    warn!("Could not drop import retries for cleared rows: {e}");
                }
            });
        }
    };

    let mut downloads_signal = props.downloads;
    let cancel_download = move |file: DownloadProgress| {
        let req = CancelDownloadRequest {
            id: file.id.clone(),
            source: file.source.clone(),
            item: file.item.clone(),
            backend: file.backend.clone(),
        };
        let item_key = file.item.clone();
        spawn(async move {
            if api::cancel_download(req).await.is_ok() {
                let mut map = downloads_signal.write();
                if let Some(entry) = map.get_mut(&item_key) {
                    entry.state = DownloadState::Cancelled;
                }
            }
        });
    };

    let mut fail =
        move |item: &str, error: &ServerFnError, from_picker: bool| match downloads_signal
            .write()
            .get_mut(item)
        {
            Some(entry) => show_failure(entry, error, from_picker),
            None => Failure::OnRow,
        };
    let import_again = use_callback(
        move |(item, choice): (String, ImportChoice)| -> Pin<Box<dyn Future<Output = Result<(), Failure>>>> {
            Box::pin(async move {
                let from_picker = sent_by_picker(&choice);
                api::retry_import(item.clone(), choice)
                    .await
                    .map_err(|e| fail(&item, &e, from_picker))
            })
        },
    );
    let load_choices = use_callback(
        move |item: String| -> Pin<Box<dyn Future<Output = Result<MatchChoices, Failure>>>> {
            Box::pin(async move {
                api::import_choices(item.clone())
                    .await
                    .map_err(|e| fail(&item, &e, true))
            })
        },
    );

    let close_modal = move |_| props.is_open.set(false);

    // Escape closes the drawer wherever focus is, as the backdrop click does.
    let mut is_open = props.is_open;
    use_effect(move || {
        spawn(async move {
            let mut keys = document::eval(
                r#"
                document.addEventListener("keydown", (event) => {
                    if (event.key === "Escape") dioxus.send(true);
                });
                await new Promise(() => {});
                "#,
            );
            while keys.recv::<bool>().await.is_ok() {
                if *is_open.peek() {
                    is_open.set(false);
                }
            }
        });
    });

    let (modal_opacity, panel_translate, pointer_events) = if (*props.is_open)() {
        ("opacity-100 visible", "translate-x-0", "pointer-events-auto")
    } else {
        // Hidden once the fade ends: nothing closed can take a click or focus.
        ("opacity-0 invisible", "translate-x-full", "pointer-events-none")
    };

    rsx! {
      div { class: "fixed inset-0 z-50 flex justify-end transition-[opacity,visibility] duration-200 ease-out {modal_opacity} {pointer_events}",
        // Backdrop
        div {
          class: "absolute inset-0 bg-black/70 cursor-pointer",
          aria_hidden: "true",
          onclick: close_modal,
        }

        // Panel
        div { class: "relative w-full max-w-md bg-beet-panel h-full shadow-2xl shadow-black md:rounded-l-lg transform transition-transform duration-200 ease-out flex flex-col {panel_translate}",
          //Header
          div { class: "p-6 flex justify-between items-center bg-gradient-to-b from-beet-dim to-beet-panel md:rounded-tl-lg",
            div {
              h3 { class: "text-2xl font-bold text-white tracking-tight",
                "Active Transfers"
              }
              p { class: "flex items-center gap-3 text-xs text-muted lowercase mt-1.5",
                span { class: "flex items-center gap-1.5",
                  span { class: "w-1.5 h-1.5 rounded-full bg-beet-leaf" }
                  "{processing_count} PROCESSING"
                }
                span { class: "flex items-center gap-1.5",
                  span {
                    class: "w-1.5 h-1.5 rounded-full",
                    class: if errored_count > 0 { "bg-red-500" } else { "bg-white/30" },
                  }
                  "{errored_count} ERRORED"
                }
              }
            }
            button {
              class: "p-1.5 rounded-full text-muted hover:text-white hover:bg-white/10 transition-colors cursor-pointer",
              aria_label: "Close downloads",
              onclick: close_modal,
              svg {
                class: "w-6 h-6",
                fill: "none",
                stroke: "currentColor",
                view_box: "0 0 24 24",
                path {
                  stroke_linecap: "round",
                  stroke_linejoin: "round",
                  stroke_width: "2",
                  d: "M6 18L18 6M6 6l12 12",
                }
              }
            }
          }

          // Content
          div { class: "flex-1 overflow-y-auto px-4 md:px-6 pb-6 no-scrollbar space-y-2",
            if groups.is_empty() {
              div { class: "text-center text-muted py-10 text-sm",
                "No active transfers in the queue."
              }
            }

            for group in groups.iter() {
              div {
                key: "{group.key}",
                class: "download-album space-y-2",
                role: "group",
                aria_label: "{group.title}",
                div { class: "flex items-baseline justify-between gap-3 pt-3 pb-0.5",
                  h4 { class: "text-xs font-semibold uppercase tracking-wider text-white truncate",
                    "{group.title}"
                  }
                  span { class: "text-xs text-muted shrink-0", {group_summary(group)} }
                }
                for file in group.rows.iter() {
                  DownloadItem {
                    key: "{file.item}",
                    file: file.clone(),
                    on_cancel: cancel_download,
                    on_import: import_again,
                    on_load_choices: load_choices,
                  }
                }
              }
            }
          }
          // Footer
          div { class: "p-4 border-t border-white/5",
            button {
              class: "pill-outline w-full py-2",
              onclick: clear_finished,
              "CLEAR COMPLETED"
            }
          }
        }
      }
    }
}

/// Where a failed request from a row shows why.
#[derive(Debug, Clone, PartialEq)]
enum Failure {
    /// On the row's reason line.
    OnRow,
    /// In the picker that sent it, which stays.
    InPicker(String),
}

/// Picks and match loads come from the picker; Retry and as-is from the row.
fn sent_by_picker(choice: &ImportChoice) -> bool {
    matches!(
        choice,
        ImportChoice::Candidate { .. } | ImportChoice::MusicBrainz(_)
    )
}

/// What a refusal says the server still keeps for the row. None when the
/// request was not refused, or the refusal does not say.
fn kept_after(error: &ServerFnError) -> Option<KeptImport> {
    match error {
        ServerFnError::ServerError {
            code: api::REFUSAL_CODE,
            details: Some(details),
            ..
        } => serde_json::from_value(details.clone()).ok(),
        _ => None,
    }
}

/// Takes away what the server says it no longer keeps for the row, and
/// puts why on the reason line, unless the picker sent the request and
/// stays, which then shows it so the skip reason stays readable.
fn show_failure(row: &mut DownloadProgress, error: &ServerFnError, from_picker: bool) -> Failure {
    match kept_after(error) {
        Some(KeptImport::Gone) => {
            row.retryable = false;
            row.can_choose_import = false;
        }
        Some(KeptImport::WithoutMatches) => row.can_choose_import = false,
        Some(KeptImport::WithMatches) | None => {}
    }
    let message = friendly_error(error);
    if from_picker && row.retryable && row.can_choose_import {
        return Failure::InPicker(message);
    }
    row.error = Some(message);
    Failure::OnRow
}

/// The folder a remote path sits in; the path may use `\` or `/`.
fn parent_folder(item: &str) -> Option<String> {
    let normalized = item.replace('\\', "/");
    std::path::Path::new(&normalized)
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().into_owned())
}

/// The file name of a remote path, without its folders.
fn file_name(item: &str) -> &str {
    item.rsplit(['/', '\\']).next().unwrap_or(item)
}

/// Compares case-insensitively, with runs of digits compared as numbers so
/// "2 - x" comes before "10 - x".
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.to_lowercase(), b.to_lowercase());
    let (mut a, mut b) = (a.as_str(), b.as_str());
    loop {
        match (a.chars().next(), b.chars().next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let end_a = a.find(|c: char| !c.is_ascii_digit()).unwrap_or(a.len());
                let end_b = b.find(|c: char| !c.is_ascii_digit()).unwrap_or(b.len());
                let num_a = a[..end_a].trim_start_matches('0');
                let num_b = b[..end_b].trim_start_matches('0');
                let ord = num_a.len().cmp(&num_b.len()).then_with(|| num_a.cmp(num_b));
                if ord != Ordering::Equal {
                    return ord;
                }
                a = &a[end_a..];
                b = &b[end_b..];
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(&y);
                }
                a = &a[x.len_utf8()..];
                b = &b[y.len_utf8()..];
            }
        }
    }
}

/// Rows that will not reach the library on their own; they go last in their group.
fn is_failed(state: &DownloadState) -> bool {
    matches!(state, DownloadState::Failed(_) | DownloadState::Cancelled)
}

/// One album or batch of rows in the drawer.
#[derive(Debug, Clone, PartialEq)]
struct RowGroup {
    key: String,
    title: String,
    rows: Vec<DownloadProgress>,
    done: usize,
    failed: usize,
}

/// Rows grouped by batch, or by folder when a row has no batch. Groups go by
/// title and rows by track, so nothing moves when a row changes state, except
/// failed and cancelled rows, which sink to the bottom of their group.
fn grouped_rows(downloads: &HashMap<String, DownloadProgress>) -> Vec<RowGroup> {
    let mut groups: HashMap<String, RowGroup> = HashMap::new();
    for row in downloads.values() {
        let folder = parent_folder(&row.item);
        let key = match (&row.batch_id, &folder) {
            (Some(id), _) => format!("batch:{id}"),
            (None, Some(folder)) => format!("folder:{folder}"),
            (None, None) => String::new(),
        };
        let group = groups.entry(key.clone()).or_insert_with(|| RowGroup {
            key,
            title: String::new(),
            rows: Vec::new(),
            done: 0,
            failed: 0,
        });
        group.rows.push(row.clone());
    }

    let mut groups: Vec<RowGroup> = groups
        .into_values()
        .map(|mut group| {
            group.rows.sort_by(|a, b| {
                is_failed(&a.state)
                    .cmp(&is_failed(&b.state))
                    .then_with(|| natural_cmp(file_name(&a.item), file_name(&b.item)))
                    .then_with(|| a.item.cmp(&b.item))
            });
            let first = &group.rows[0];
            group.title = group
                .rows
                .iter()
                .find_map(|r| r.batch_label.clone())
                .or_else(|| parent_folder(&first.item))
                .unwrap_or_else(|| "Other".to_string());
            group.done = group
                .rows
                .iter()
                .filter(|r| r.state == DownloadState::Imported)
                .count();
            group.failed = group.rows.iter().filter(|r| is_failed(&r.state)).count();
            group
        })
        .collect();
    groups.sort_by(|a, b| {
        a.title
            .to_lowercase()
            .cmp(&b.title.to_lowercase())
            .then_with(|| a.key.cmp(&b.key))
    });
    groups
}

/// The line under a group's title: how many rows are done, and how many failed.
fn group_summary(group: &RowGroup) -> String {
    let mut summary = format!("{}/{} done", group.done, group.rows.len());
    if group.failed > 0 {
        summary.push_str(&format!(" · {} failed", group.failed));
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use dioxus::fullstack::RequestError;

    fn row(item: &str, state: DownloadState) -> (String, DownloadProgress) {
        let p =
            DownloadProgress::queued(item.into(), "peer".into(), item.into(), 1).with_state(state);
        (item.to_string(), p)
    }

    fn batch_row(item: &str, state: DownloadState, batch: &str) -> (String, DownloadProgress) {
        let (key, p) = row(item, state);
        (key, p.with_batch(batch.into(), format!("{batch} album")))
    }

    fn rows_of(group: &RowGroup) -> Vec<&str> {
        group.rows.iter().map(|r| r.item.as_str()).collect()
    }

    #[test]
    fn interleaved_albums_come_out_as_groups_in_title_order() {
        let map: HashMap<String, DownloadProgress> = [
            batch_row("z/02 - two.flac", DownloadState::InProgress, "zeta"),
            batch_row("a/01 - one.flac", DownloadState::Queued, "alpha"),
            batch_row("z/01 - one.flac", DownloadState::Imported, "zeta"),
            batch_row("a/02 - two.flac", DownloadState::Imported, "alpha"),
        ]
        .into_iter()
        .collect();
        let groups = grouped_rows(&map);
        let titles: Vec<&str> = groups.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(titles, ["alpha album", "zeta album"]);
        assert_eq!(rows_of(&groups[0]), ["a/01 - one.flac", "a/02 - two.flac"]);
        assert_eq!(rows_of(&groups[1]), ["z/01 - one.flac", "z/02 - two.flac"]);
    }

    #[test]
    fn tracks_sort_by_number_not_text() {
        let map: HashMap<String, DownloadProgress> = [
            row("music\\Band\\LP\\10 - b.flac", DownloadState::Queued),
            row("music\\Band\\LP\\2 - a.flac", DownloadState::Queued),
            row("music\\Band\\LP\\1 - c.flac", DownloadState::Queued),
        ]
        .into_iter()
        .collect();
        let groups = grouped_rows(&map);
        assert_eq!(groups.len(), 1);
        assert_eq!(
            rows_of(&groups[0]),
            [
                "music\\Band\\LP\\1 - c.flac",
                "music\\Band\\LP\\2 - a.flac",
                "music\\Band\\LP\\10 - b.flac",
            ]
        );
    }

    #[test]
    fn failed_and_cancelled_rows_sink_to_the_bottom_of_their_group() {
        let map: HashMap<String, DownloadProgress> = [
            batch_row("LP/01 - a.flac", DownloadState::Failed("x".into()), "b"),
            batch_row("LP/02 - b.flac", DownloadState::Cancelled, "b"),
            batch_row("LP/03 - c.flac", DownloadState::Imported, "b"),
            batch_row("LP/04 - d.flac", DownloadState::InProgress, "b"),
            batch_row("LP/05 - e.flac", DownloadState::Queued, "b"),
        ]
        .into_iter()
        .collect();
        let groups = grouped_rows(&map);
        assert_eq!(
            rows_of(&groups[0]),
            [
                "LP/03 - c.flac",
                "LP/04 - d.flac",
                "LP/05 - e.flac",
                "LP/01 - a.flac",
                "LP/02 - b.flac",
            ]
        );
    }

    #[test]
    fn a_finished_download_keeps_its_place() {
        let mut map: HashMap<String, DownloadProgress> = [
            batch_row("LP/01 - a.flac", DownloadState::Queued, "b"),
            batch_row("LP/02 - b.flac", DownloadState::InProgress, "b"),
            batch_row("LP/03 - c.flac", DownloadState::Queued, "b"),
        ]
        .into_iter()
        .collect();
        let before: Vec<String> = grouped_rows(&map)[0]
            .rows
            .iter()
            .map(|r| r.item.clone())
            .collect();
        map.get_mut("LP/02 - b.flac").unwrap().state = DownloadState::Imported;
        let after: Vec<String> = grouped_rows(&map)[0]
            .rows
            .iter()
            .map(|r| r.item.clone())
            .collect();
        assert_eq!(before, after);
        assert_eq!(after[1], "LP/02 - b.flac");
    }

    #[test]
    fn rows_without_a_batch_group_by_their_folder() {
        let map: HashMap<String, DownloadProgress> = [
            row(
                "music\\Static Harbor\\Glass Atlas\\01 - Northern Line.flac",
                DownloadState::Queued,
            ),
            row("music/Other Band/Blue/01 - x.flac", DownloadState::Queued),
            row(
                "music\\Static Harbor\\Glass Atlas\\02 - South.flac",
                DownloadState::Queued,
            ),
            row("loose.flac", DownloadState::Queued),
        ]
        .into_iter()
        .collect();
        let groups = grouped_rows(&map);
        let titles: Vec<&str> = groups.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(titles, ["Blue", "Glass Atlas", "Other"]);
        assert_eq!(groups[1].rows.len(), 2);
        assert_eq!(rows_of(&groups[2]), ["loose.flac"]);
    }

    #[test]
    fn groups_count_done_and_failed_rows() {
        let map: HashMap<String, DownloadProgress> = [
            batch_row("LP/01 - a.flac", DownloadState::Failed("x".into()), "b"),
            batch_row("LP/02 - b.flac", DownloadState::Cancelled, "b"),
            batch_row("LP/03 - c.flac", DownloadState::Imported, "b"),
            batch_row("LP/04 - d.flac", DownloadState::Imported, "b"),
            batch_row("LP/05 - e.flac", DownloadState::InProgress, "b"),
        ]
        .into_iter()
        .collect();
        let group = &grouped_rows(&map)[0];
        assert_eq!((group.done, group.failed), (2, 2));
        assert_eq!(group.rows.len(), 5);
        assert_eq!(group_summary(group), "2/5 done · 2 failed");
    }

    #[test]
    fn a_group_with_nothing_failed_only_counts_done() {
        let map: HashMap<String, DownloadProgress> = [
            batch_row("LP/01 - a.flac", DownloadState::Imported, "b"),
            batch_row("LP/02 - b.flac", DownloadState::Queued, "b"),
        ]
        .into_iter()
        .collect();
        assert_eq!(group_summary(&grouped_rows(&map)[0]), "1/2 done");
    }

    fn kept_skip() -> DownloadProgress {
        DownloadProgress {
            error: Some("No close match (2 candidates)".into()),
            retryable: true,
            can_choose_import: true,
            ..row("a", DownloadState::ImportSkipped).1
        }
    }

    /// A refusal as the client gets it: built by the server's helper and
    /// sent through JSON.
    fn refused(message: &str, kept: KeptImport) -> ServerFnError {
        let sent = serde_json::to_value(api::refusal_error_with(message, kept)).unwrap();
        serde_json::from_value(sent).unwrap()
    }

    fn every_choice() -> [ImportChoice; 4] {
        [
            ImportChoice::Auto,
            ImportChoice::AsIs,
            ImportChoice::Candidate {
                data_source: "MusicBrainz".into(),
                id: "r1".into(),
            },
            ImportChoice::MusicBrainz("https://musicbrainz.org/release/x".into()),
        ]
    }

    fn after(choice: &ImportChoice, error: &ServerFnError) -> (DownloadProgress, Failure) {
        let mut row = kept_skip();
        let failure = show_failure(&mut row, error, sent_by_picker(choice));
        (row, failure)
    }

    #[test]
    fn the_server_says_what_a_refusal_keeps() {
        let kept = KeptImport::WithoutMatches;
        assert_eq!(kept_after(&refused("x", kept)), Some(kept));
        let plain = api::refusal_error("Folder not found");
        assert_eq!(kept_after(&plain), None);
    }

    #[test]
    fn a_refusal_that_dropped_the_job_takes_every_action_away() {
        for message in ["Nothing to retry", "The kept files are gone"] {
            for choice in every_choice() {
                let (row, failure) = after(&choice, &refused(message, KeptImport::Gone));
                assert_eq!(failure, Failure::OnRow, "{choice:?}");
                assert_eq!(row.error.as_deref(), Some(message));
                assert!(!row.retryable && !row.can_choose_import, "{choice:?}");
            }
        }
    }

    #[test]
    fn a_refusal_without_matches_keeps_only_retry() {
        let message = "This import has no matches to pick from";
        for choice in every_choice() {
            let (row, failure) = after(&choice, &refused(message, KeptImport::WithoutMatches));
            assert_eq!(failure, Failure::OnRow, "{choice:?}");
            assert_eq!(row.error.as_deref(), Some(message));
            assert!(row.retryable && !row.can_choose_import, "{choice:?}");
        }
        // A match load refused the same way takes the picker too.
        let mut row = kept_skip();
        show_failure(
            &mut row,
            &refused(message, KeptImport::WithoutMatches),
            true,
        );
        assert!(row.retryable && !row.can_choose_import);
    }

    #[test]
    fn a_refused_pick_keeps_every_action_and_shows_why_in_the_picker() {
        let text = "Track imports need a MusicBrainz recording, not a release";
        for choice in every_choice().into_iter().filter(sent_by_picker) {
            let (row, failure) = after(&choice, &refused(text, KeptImport::WithMatches));
            assert_eq!(failure, Failure::InPicker(text.into()), "{choice:?}");
            assert_eq!(row.error.as_deref(), Some("No close match (2 candidates)"));
            assert!(row.retryable && row.can_choose_import);
        }
    }

    #[test]
    fn an_unreachable_server_changes_no_action() {
        let error = ServerFnError::Request(RequestError::Request("connection refused".into()));
        let text = "Could not reach the server. Check your connection.";
        let (row, failure) = after(&ImportChoice::Auto, &error);
        assert_eq!(failure, Failure::OnRow);
        assert_eq!(row.error.as_deref(), Some(text));
        assert!(row.retryable && row.can_choose_import);

        // A failed pick or match load keeps the skip reason.
        let mut row = kept_skip();
        assert_eq!(
            show_failure(&mut row, &error, true),
            Failure::InPicker(text.into())
        );
        assert_eq!(row, kept_skip());
    }
}
