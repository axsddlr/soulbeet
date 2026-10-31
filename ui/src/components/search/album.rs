use api::models::folder::Folder;
use dioxus::prelude::*;
use shared::metadata::{Album, Track};
use std::collections::HashMap;

use super::download_icon::{DownloadIcon, DownloadRowState};
use super::inline_track_panel::InlineTrackPanel;
use crate::CoverArt;

#[derive(Props, PartialEq, Clone)]
pub struct Props {
    pub album: Album,
    pub is_expanded: bool,
    pub on_toggle: Callback,
    pub on_search_sources: Callback,
    pub download_state: DownloadRowState,
    pub folders: Vec<Folder>,
    pub selected_folder_id: Option<String>,
    pub active_menu: Signal<Option<String>>,
    pub tracks: Option<Vec<Track>>,
    pub download_states: Signal<HashMap<String, DownloadRowState>>,
    #[props(into)]
    pub on_download: EventHandler<()>,
    #[props(into)]
    pub on_override_download: EventHandler<Folder>,
    #[props(into)]
    pub on_track_download: EventHandler<Track>,
    #[props(into)]
    pub on_track_override_download: EventHandler<(Track, Folder)>,
}

/// The year of a release date, or the date as given when it does not start
/// with one.
fn year(date: &str) -> &str {
    match date.get(..4) {
        Some(y) if y.chars().all(|c| c.is_ascii_digit()) => y,
        _ => date,
    }
}

#[component]
pub fn AlbumResult(props: Props) -> Element {
    let album = &props.album;
    let year = album.release_date.as_deref().map(year);
    let track_count = props
        .tracks
        .as_ref()
        .filter(|t| !t.is_empty())
        .map(|t| t.len());

    let search_sources = rsx! {
        button {
            class: "p-2 rounded-full text-muted hover:text-white hover:bg-white/10 transition-colors cursor-pointer group/src",
            title: "Search sources",
            onclick: move |evt: MouseEvent| {
                evt.stop_propagation();
                props.on_search_sources.call(());
            },
            svg {
                class: "w-5 h-5",
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
    };

    let download = rsx! {
        DownloadIcon {
            item_id: props.album.id.clone(),
            state: props.download_state.clone(),
            folders: props.folders.clone(),
            selected_folder_id: props.selected_folder_id.clone(),
            active_menu: props.active_menu,
            on_download: props.on_download,
            on_override_download: props.on_override_download,
            filled: true,
        }
    };

    rsx! {
        div {
            class: "h-full rounded-lg transition-colors duration-300",
            class: if props.is_expanded { "bg-gradient-to-b from-beet-dim to-beet-panel overflow-visible" } else { "bg-beet-panel hover:bg-beet-dim" },

            if props.is_expanded {
                // Album header, Spotify style
                div {
                    class: "flex flex-col sm:flex-row sm:items-end gap-4 md:gap-6 p-4 md:p-6 cursor-pointer group",
                    onclick: move |_| props.on_toggle.call(()),
                    CoverArt {
                        album: album.clone(),
                        class: "w-40 h-40 md:w-[232px] md:h-[232px] rounded shadow-2xl shadow-black/60",
                    }
                    div { class: "min-w-0 flex-1",
                        p { class: "text-xs font-bold text-white mb-1 md:mb-2", "Album" }
                        p { class: "text-3xl md:text-5xl lg:text-7xl font-extrabold tracking-tight text-white leading-none break-words line-clamp-2 pb-1",
                            "{album.title}"
                        }
                        p { class: "mt-3 md:mt-5 text-sm text-muted truncate",
                            span { class: "font-bold text-white", "{album.artist}" }
                            if let Some(year) = year {
                                " · {year}"
                            }
                            if let Some(n) = track_count {
                                " · {n} tracks"
                            }
                        }
                    }
                }
                // Action row
                div { class: "flex items-center gap-2 px-4 md:px-6 pb-4",
                    {download}
                    {search_sources}
                    button {
                        class: "p-2 rounded-full text-muted hover:text-white transition-colors cursor-pointer",
                        title: "Collapse",
                        onclick: move |_| props.on_toggle.call(()),
                        svg {
                            class: "w-5 h-5 -rotate-90",
                            fill: "none",
                            stroke: "currentColor",
                            stroke_width: "2",
                            view_box: "0 0 24 24",
                            path {
                                stroke_linecap: "round",
                                stroke_linejoin: "round",
                                d: "M8.25 4.5l7.5 7.5-7.5 7.5",
                            }
                        }
                    }
                }
            } else {
                // Album card
                div {
                    class: "p-3 group cursor-pointer",
                    onclick: move |_| props.on_toggle.call(()),
                    div { class: "relative mb-3",
                        CoverArt {
                            album: album.clone(),
                            class: "w-full aspect-square rounded-md shadow-lg shadow-black/50",
                        }
                        // Round green action, shown on hover like Spotify's play button
                        div { class: "absolute right-2 bottom-2 z-10 transition-opacity duration-200 md:opacity-0 md:group-hover:opacity-100 focus-within:opacity-100",
                            {download}
                        }
                    }
                    div { class: "flex items-start gap-1",
                        div { class: "min-w-0 flex-1",
                            p { class: "text-sm md:text-base font-bold text-white truncate",
                                "{album.title}"
                            }
                            p { class: "text-xs md:text-sm text-muted truncate mt-1",
                                if let Some(year) = year {
                                    "{year} · "
                                }
                                "{album.artist}"
                            }
                        }
                        {search_sources}
                    }
                }
            }

            // Expansion panel (always in DOM per NFR-02)
            div {
                class: "grid transition-[grid-template-rows] duration-300 ease-in-out",
                class: if props.is_expanded { "grid-rows-[1fr]" } else { "grid-rows-[0fr]" },
                div {
                    class: "overflow-hidden",
                    InlineTrackPanel {
                        tracks: if props.is_expanded { props.tracks.clone() } else { Some(vec![]) },
                        download_states: props.download_states,
                        folders: props.folders.clone(),
                        selected_folder_id: props.selected_folder_id.clone(),
                        active_menu: props.active_menu,
                        on_track_download: props.on_track_download,
                        on_track_override_download: props.on_track_override_download,
                    }
                }
            }
        }
    }
}
