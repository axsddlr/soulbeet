use std::collections::HashMap;

use dioxus::prelude::*;
use shared::download::{DownloadProgress, DownloadState};

/// Rows still on their way into the library, as the drawer counts them.
fn is_active(state: &DownloadState) -> bool {
    matches!(
        state,
        DownloadState::Queued
            | DownloadState::InProgress
            | DownloadState::ImportQueued(_)
            | DownloadState::Importing
            | DownloadState::Completed
    )
}

/// Which active row the bar names: the one moving bytes first, then the one
/// being imported, then the ones waiting.
fn rank(state: &DownloadState) -> u8 {
    match state {
        DownloadState::InProgress => 0,
        DownloadState::Importing => 1,
        DownloadState::Completed => 2,
        DownloadState::ImportQueued(_) => 3,
        _ => 4,
    }
}

/// What the bar shows of the queue.
#[derive(Debug, PartialEq)]
struct Activity {
    current: Option<DownloadProgress>,
    active: usize,
    failed: usize,
}

fn activity(downloads: &HashMap<String, DownloadProgress>) -> Activity {
    let current = downloads
        .values()
        .filter(|r| is_active(&r.state))
        .min_by(|a, b| {
            rank(&a.state)
                .cmp(&rank(&b.state))
                .then_with(|| b.percent.total_cmp(&a.percent))
                .then_with(|| a.item.cmp(&b.item))
        })
        .cloned();
    Activity {
        current,
        active: downloads.values().filter(|r| is_active(&r.state)).count(),
        failed: downloads
            .values()
            .filter(|r| matches!(r.state, DownloadState::Failed(_)))
            .count(),
    }
}

/// The parts of the queue summary: how many rows are active, how many failed.
fn summary_parts(activity: &Activity) -> Vec<String> {
    let mut parts = Vec::new();
    if activity.active > 0 {
        parts.push(format!("{} downloading", activity.active));
    }
    if activity.failed > 0 {
        parts.push(format!("{} failed", activity.failed));
    }
    if parts.is_empty() {
        parts.push("Queue finished".to_string());
    }
    parts
}

fn summary(activity: &Activity) -> String {
    summary_parts(activity).join(" · ")
}

/// The bar's accessible name: what it does, then the summary it shows.
fn spoken_label(activity: &Activity) -> String {
    format!("Open transfers: {}", summary_parts(activity).join(", "))
}

/// The file name without its folders or extension, and the folder it sits in.
fn title_and_folder(row: &DownloadProgress) -> (String, Option<String>) {
    let normalized = row.item.replace('\\', "/");
    let path = std::path::Path::new(&normalized);
    let title = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| row.item.clone());
    let folder = row.batch_label.clone().or_else(|| {
        path.parent()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
    });
    (title, folder)
}

fn status_text(state: &DownloadState) -> &'static str {
    match state {
        DownloadState::InProgress => "Downloading",
        DownloadState::Importing => "Importing",
        DownloadState::Completed => "Waiting to import",
        DownloadState::ImportQueued(_) => "Waiting for library",
        _ => "Queued",
    }
}

/// The bar along the bottom of the shell that sums up the transfer queue,
/// like a now-playing bar. Hidden while the queue is empty; opens the
/// transfers drawer when clicked.
#[component]
pub fn ActivityBar(
    downloads: Signal<HashMap<String, DownloadProgress>>,
    on_open: EventHandler<()>,
) -> Element {
    let map = downloads.read();
    if map.is_empty() {
        return rsx! {};
    }
    let now = activity(&map);
    let summary = summary(&now);
    let label = spoken_label(&now);

    let (title, folder, status, percent) = match &now.current {
        Some(row) => {
            let (title, folder) = title_and_folder(row);
            let percent = row.percent.clamp(0.0, 100.0) as u32;
            (title, folder, Some(status_text(&row.state)), percent)
        }
        None => ("No active downloads".to_string(), None, None, 0),
    };
    let busy = now.current.is_some();
    let tile_class = if busy {
        "bg-beet-dim text-beet-accent"
    } else {
        "bg-beet-dim text-muted"
    };
    let dot_class = if now.failed > 0 {
        Some("bg-red-500")
    } else if busy {
        Some("bg-beet-leaf")
    } else {
        None
    };

    rsx! {
      div { class: "relative z-30 h-16 md:h-full w-full bg-black border-t border-white/5 md:border-t-0",
        button {
          r#type: "button",
          class: "group/bar w-full h-full grid grid-cols-[minmax(0,1fr)_auto] md:grid-cols-[minmax(0,3fr)_minmax(0,4fr)_minmax(0,3fr)] items-center gap-3 md:gap-6 px-3 md:px-4 text-left cursor-pointer hover:bg-white/[0.03] focus-visible:outline-2 focus-visible:outline-white focus-visible:-outline-offset-2 transition-colors",
          aria_label: "{label}",
          onclick: move |_| on_open.call(()),

          // Left: an icon tile where Spotify shows the cover, then title and folder
          div { class: "flex items-center gap-3 min-w-0",
            div { class: "w-10 h-10 md:w-14 md:h-14 shrink-0 rounded {tile_class} flex items-center justify-center shadow-lg shadow-black/50",
              svg {
                class: "w-5 h-5 md:w-6 md:h-6",
                class: if busy { "animate-pulse" },
                fill: "none",
                stroke: "currentColor",
                stroke_width: "2",
                view_box: "0 0 24 24",
                path {
                  stroke_linecap: "round",
                  stroke_linejoin: "round",
                  d: "M4 16v1a3 3 0 003 3h10a3 3 0 003-3v-1m-4-4l-4 4m0 0l-4-4m4 4V4",
                }
              }
            }
            div { class: "min-w-0",
              p { class: "text-sm font-semibold text-white truncate", "{title}" }
              p { class: "text-xs text-muted truncate mt-0.5",
                if let Some(status) = status {
                  span { "{status}" }
                }
                if let Some(folder) = folder {
                  span { " · {folder}" }
                }
              }
            }
          }

          // Center: the summary and percent above the progress line
          div { class: "hidden md:flex flex-col justify-center gap-2 w-full max-w-[722px] justify-self-center",
            div { class: "flex items-center justify-between gap-3 text-xs text-muted",
              span { class: "flex items-center gap-2 min-w-0",
                if let Some(dot) = dot_class {
                  span { class: "w-1.5 h-1.5 shrink-0 rounded-full {dot}" }
                }
                span { class: "truncate", "{summary}" }
              }
              span { class: "shrink-0 tabular-nums",
                if busy { "{percent}%" }
              }
            }
            div { class: "h-1 w-full rounded-full bg-white/10 overflow-hidden",
              div {
                class: "h-full rounded-full bg-beet-accent transition-all duration-300",
                style: "width: {percent}%",
              }
            }
          }

          // Right: where the drawer opens
          div { class: "flex items-center justify-end gap-2 min-w-0 text-xs text-muted group-hover/bar:text-white transition-colors",
            // Phones have no middle column: the summary sits here.
            span { class: "md:hidden flex items-center gap-1.5 whitespace-nowrap",
              if let Some(dot) = dot_class {
                span { class: "w-1.5 h-1.5 shrink-0 rounded-full {dot}" }
              }
              "{summary}"
            }
            span { class: "hidden md:inline font-semibold", "Transfers" }
            svg {
              class: "w-5 h-5 shrink-0",
              fill: "none",
              stroke: "currentColor",
              stroke_width: "2",
              view_box: "0 0 24 24",
              path {
                stroke_linecap: "round",
                stroke_linejoin: "round",
                d: "M4 6h16M4 12h16M4 18h10",
              }
            }
          }
        }

        // Phones have no room for the middle bar: a hairline along the bottom.
        div { class: "md:hidden pointer-events-none absolute left-2 right-2 bottom-0 h-0.5 rounded-full bg-white/10 overflow-hidden",
          div {
            class: "h-full bg-beet-accent transition-all duration-300",
            style: "width: {percent}%",
          }
        }

        // Reads queue changes out; the button's name is only read on focus.
        p { class: "sr-only", aria_live: "polite", "{summary}" }
      }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::download::DownloadState;

    fn row(item: &str, state: DownloadState, percent: f64) -> (String, DownloadProgress) {
        let mut p =
            DownloadProgress::queued(item.into(), "peer".into(), item.into(), 1).with_state(state);
        p.percent = percent;
        (item.to_string(), p)
    }

    fn render(rows: Vec<(String, DownloadProgress)>) -> String {
        let mut dom = VirtualDom::new_with_props(
            |rows: Vec<(String, DownloadProgress)>| {
                let downloads = use_signal(|| rows.into_iter().collect::<HashMap<_, _>>());
                rsx! {
                    ActivityBar { downloads, on_open: |_| {} }
                }
            },
            rows,
        );
        dom.rebuild_in_place();
        dioxus_ssr::render(&dom)
    }

    const LANTERNS: &str = "Music\\FLAC\\Static Harbor\\Glass Atlas\\02 - Paper Lanterns.flac";

    #[test]
    fn an_empty_queue_shows_no_bar() {
        let html = render(vec![]);
        assert!(!html.contains("<button"), "{html}");
        assert!(!html.contains("downloading"), "{html}");
    }

    #[test]
    fn one_active_download_shows_its_title_and_folder() {
        let html = render(vec![row(LANTERNS, DownloadState::InProgress, 10.0)]);
        assert!(html.contains("02 - Paper Lanterns<"), "{html}");
        assert!(html.contains("Glass Atlas"), "{html}");
        assert!(html.contains("1 downloading"), "{html}");
        assert!(!html.contains("failed"), "{html}");
    }

    #[test]
    fn mixed_states_sum_up_downloading_and_failed() {
        let html = render(vec![
            row("a.flac", DownloadState::InProgress, 50.0),
            row("b.flac", DownloadState::Queued, 0.0),
            row("c.flac", DownloadState::Importing, 100.0),
            row("d.flac", DownloadState::Failed("x".into()), 0.0),
            row("e.flac", DownloadState::Imported, 100.0),
        ]);
        assert!(html.contains("3 downloading · 1 failed"), "{html}");
        // The bar names the transfer that is moving bytes.
        assert!(html.contains(">a<"), "{html}");
    }

    #[test]
    fn the_progress_bar_is_as_wide_as_the_percent() {
        let html = render(vec![row(LANTERNS, DownloadState::InProgress, 42.4)]);
        assert!(html.contains("width: 42%"), "{html}");
    }

    #[test]
    fn the_bar_is_named_after_its_summary_and_reads_it_out() {
        let html = render(vec![
            row("a.flac", DownloadState::InProgress, 50.0),
            row("b.flac", DownloadState::Queued, 0.0),
            row("c.flac", DownloadState::Importing, 100.0),
            row("d.flac", DownloadState::Failed("x".into()), 0.0),
        ]);
        assert!(
            html.contains(r#"aria-label="Open transfers: 3 downloading, 1 failed""#),
            "{html}"
        );
        assert!(
            html.contains(r#"aria-live="polite">3 downloading · 1 failed<"#),
            "{html}"
        );
    }

    #[test]
    fn a_queue_with_nothing_active_shows_the_idle_state() {
        let html = render(vec![
            row("a.flac", DownloadState::Imported, 100.0),
            row("b.flac", DownloadState::Failed("x".into()), 0.0),
        ]);
        assert!(html.contains("No active downloads"), "{html}");
        assert!(html.contains("1 failed"), "{html}");
        assert!(!html.contains("downloading"), "{html}");
    }
}
