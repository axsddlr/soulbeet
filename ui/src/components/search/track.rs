use api::models::folder::Folder;
use dioxus::prelude::*;
use shared::metadata::Track;

use super::download_icon::{DownloadIcon, DownloadRowState};

#[derive(Props, PartialEq, Clone)]
pub struct Props {
    pub track: Track,
    /// Position in the result list, from 0; shown 1-based in the # column.
    #[props(default)]
    pub index: usize,
    pub on_album_click: Callback,
    pub on_search_sources: Callback,
    pub download_state: DownloadRowState,
    pub folders: Vec<Folder>,
    pub selected_folder_id: Option<String>,
    pub active_menu: Signal<Option<String>>,
    #[props(into)]
    pub on_download: EventHandler<()>,
    #[props(into)]
    pub on_override_download: EventHandler<Folder>,
}

#[component]
pub fn TrackResult(props: Props) -> Element {
    let track = props.track.clone();
    let mut cover_error = use_signal(|| false);

    let cover_url = track
        .release_mbid
        .as_ref()
        .map(|mbid| format!("https://coverartarchive.org/release/{}/front-250", mbid));

    let number = props.index + 1;

    rsx! {
      div {
        class: "group grid grid-cols-[minmax(0,1fr)_auto] md:grid-cols-[2rem_minmax(0,4fr)_minmax(0,3fr)_4rem_4.5rem] items-center gap-3 md:gap-4 px-2 md:px-4 py-2 rounded-md hover:bg-white/10 transition-colors",

        // Number
        span { class: "hidden md:block text-sm text-muted text-right tabular-nums", "{number}" }

        // Cover thumb, title and artist
        div { class: "flex items-center gap-3 min-w-0",
          div {
            class: "w-10 h-10 flex-shrink-0 bg-beet-dim rounded flex items-center justify-center overflow-hidden cursor-pointer",
            onclick: move |_| props.on_album_click.call(()),
            if let Some(url) = cover_url.filter(|_| !cover_error()) {
              img {
                src: "{url}",
                alt: "Cover",
                class: "w-full h-full object-cover",
                onerror: move |_| cover_error.set(true),
              }
            } else {
              svg {
                class: "w-5 h-5 text-white/30",
                fill: "none",
                stroke: "currentColor",
                stroke_width: "1.5",
                view_box: "0 0 24 24",
                path {
                  stroke_linecap: "round",
                  stroke_linejoin: "round",
                  d: "M9 9l10.5-3m0 6.553v3.75a2.25 2.25 0 01-1.632 2.163l-1.32.377a1.803 1.803 0 11-.99-3.467l2.31-.66a2.25 2.25 0 001.632-2.163zm0 0V2.25L9 5.25v10.303m0 0v3.75a2.25 2.25 0 01-1.632 2.163l-1.32.377a1.803 1.803 0 01-.99-3.467l2.31-.66A2.25 2.25 0 009 15.553z",
                }
              }
            }
          }
          div { class: "min-w-0",
            p { class: "text-sm md:text-base font-medium text-white truncate", "{track.title}" }
            p { class: "text-xs md:text-sm text-muted truncate group-hover:text-white transition-colors",
              "{track.artist}"
              if let Some(album_title) = &track.album_title {
                span { class: "md:hidden", " · {album_title}" }
              }
            }
          }
        }

        // Album
        div { class: "hidden md:block min-w-0",
          if let Some(album_title) = &track.album_title {
            p {
              class: "text-sm text-muted truncate cursor-pointer hover:text-white hover:underline transition-colors",
              onclick: move |_| props.on_album_click.call(()),
              "{album_title}"
            }
          }
        }

        // Duration
        span { class: "hidden md:block text-sm text-muted text-right tabular-nums whitespace-nowrap",
          if let Some(duration) = &track.duration {
            "{duration}"
          }
        }

        // Action buttons
        div { class: "flex items-center justify-end gap-1 shrink-0",
          // Search sources button
          button {
            class: "p-2 rounded-full text-muted hover:text-white hover:bg-white/10 transition-colors cursor-pointer md:opacity-0 md:group-hover:opacity-100 focus:opacity-100 group/src",
            title: "Search sources",
            onclick: move |evt: MouseEvent| {
              evt.stop_propagation();
              props.on_search_sources.call(());
            },
            svg {
              class: "w-4 h-4",
              fill: "none",
              stroke: "currentColor",
              stroke_width: "2",
              view_box: "0 0 24 24",
              path {
                stroke_linecap: "round",
                stroke_linejoin: "round",
                d: "M21 21l-6-6m2-5a7 7 0 11-14 0 7 7 0 0114 0z",
              }
            }
          }

          // Auto-download button
          DownloadIcon {
            item_id: props.track.id.clone(),
            state: props.download_state.clone(),
            folders: props.folders.clone(),
            selected_folder_id: props.selected_folder_id.clone(),
            active_menu: props.active_menu,
            on_download: props.on_download,
            on_override_download: props.on_override_download,
          }
        }
      }
    }
}
