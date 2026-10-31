use api::models::folder::Folder;
use dioxus::prelude::*;
use shared::metadata::Track;
use std::collections::HashMap;

use super::download_icon::DownloadRowState;
use super::inline_track_row::InlineTrackRow;

const SKELETON_WIDTHS: [u8; 4] = [60, 75, 50, 65];

#[derive(Props, PartialEq, Clone)]
pub struct Props {
    pub tracks: Option<Vec<Track>>,
    pub download_states: Signal<HashMap<String, DownloadRowState>>,
    pub folders: Vec<Folder>,
    pub selected_folder_id: Option<String>,
    pub active_menu: Signal<Option<String>>,
    #[props(into)]
    pub on_track_download: EventHandler<Track>,
    #[props(into)]
    pub on_track_override_download: EventHandler<(Track, Folder)>,
}

#[component]
pub fn InlineTrackPanel(props: Props) -> Element {
    let has_folders = !props.folders.is_empty();

    rsx! {
        div {
            class: "px-2 md:px-4 pb-4",

            match &props.tracks {
                None => rsx! {
                    for width in SKELETON_WIDTHS {
                        div {
                            class: "flex items-center gap-3 p-2",
                            div { class: "w-6 h-4 bg-white/5 rounded animate-pulse" }
                            div {
                                class: "flex-grow h-4 bg-white/5 rounded animate-pulse",
                                style: "max-width: {width}%",
                            }
                            div { class: "w-10 h-4 bg-white/5 rounded animate-pulse" }
                            div { class: "w-5 h-5 bg-white/5 rounded-full animate-pulse" }
                        }
                    }
                },
                Some(tracks) if tracks.is_empty() => rsx! {
                    p {
                        class: "text-sm text-muted text-center py-4",
                        "No tracks found for this album."
                    }
                },
                Some(tracks) => {
                    rsx! {
                        div {
                            class: "max-h-[480px] overflow-y-auto",
                            div { class: "grid grid-cols-[1.5rem_minmax(0,1fr)_auto_2.25rem] items-center gap-3 md:gap-4 px-2 pb-2 mb-2 border-b border-white/10 text-xs font-medium uppercase tracking-wider text-muted",
                                span { class: "text-right", "#" }
                                span { "Title" }
                                svg {
                                    class: "w-4 h-4",
                                    fill: "none",
                                    stroke: "currentColor",
                                    stroke_width: "2",
                                    view_box: "0 0 24 24",
                                    path {
                                        stroke_linecap: "round",
                                        stroke_linejoin: "round",
                                        d: "M12 6v6h4.5m4.5 0a9 9 0 11-18 0 9 9 0 0118 0z",
                                    }
                                }
                                span {}
                            }
                            for (idx, track) in tracks.iter().enumerate() {
                                {
                                    let dl_state = if !has_folders {
                                        DownloadRowState::Disabled
                                    } else {
                                        props.download_states.read().get(&track.id).cloned().unwrap_or_default()
                                    };
                                    let track_for_dl = track.clone();
                                    let track_for_override = track.clone();
                                    rsx! {
                                        InlineTrackRow {
                                            key: "{track.id}",
                                            track: track.clone(),
                                            index: idx,
                                            download_state: dl_state,
                                            folders: props.folders.clone(),
                                            selected_folder_id: props.selected_folder_id.clone(),
                                            active_menu: props.active_menu,
                                            on_download: move |_| {
                                                props.on_track_download.call(track_for_dl.clone());
                                            },
                                            on_override_download: move |folder: Folder| {
                                                props.on_track_override_download.call((track_for_override.clone(), folder));
                                            },
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
