use std::collections::HashSet;

use dioxus::prelude::*;
use shared::metadata::Track;

use crate::{album::track_item::TrackItem, Checkbox};

#[derive(Props, PartialEq, Clone)]
pub struct Props {
    tracks: Signal<Vec<Track>>,
    selected_tracks: Signal<HashSet<String>>,
    on_toggle_select_all: EventHandler,
    on_track_toggle: EventHandler<String>,
    all_selected: bool,
}

#[component]
pub fn TrackList(props: Props) -> Element {
    rsx! {
      ul { class: "list-none px-2 md:px-4 py-4 space-y-0.5 overflow-y-auto",
        // Column header row; also toggles the whole selection
        li {
          class: "flex items-center gap-4 px-3 py-2 mb-2 border-b border-white/10 cursor-pointer text-xs font-medium uppercase tracking-wider text-muted hover:text-white transition-colors",
          onclick: move |_| props.on_toggle_select_all.call(()),
          Checkbox { is_selected: props.all_selected }
          span { class: "flex-grow", "Select / Deselect All" }
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
        }
        for track in props.tracks.read().iter() {
          TrackItem {
            key: "{track.id}",
            track: track.clone(),
            is_selected: props.selected_tracks.read().contains(&track.id),
            on_toggle: props.on_track_toggle,
          }
        }
      }
    }
}
