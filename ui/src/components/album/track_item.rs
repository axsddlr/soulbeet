use dioxus::prelude::*;
use shared::metadata::Track;

use crate::Checkbox;

#[derive(Props, Clone, PartialEq)]
pub struct Props {
    track: Track,
    is_selected: bool,
    on_toggle: EventHandler<String>,
}

#[component]
pub fn TrackItem(props: Props) -> Element {
    let track_id = props.track.id.clone();

    rsx! {
      li {
        class: "flex items-center gap-4 px-3 py-2 rounded-md cursor-pointer transition-colors",
        class: if props.is_selected { "bg-white/10" } else { "hover:bg-white/10" },
        onclick: move |_| props.on_toggle.call(track_id.clone()),
        Checkbox { is_selected: props.is_selected }

        span {
          class: "flex-grow text-sm md:text-base truncate",
          class: if props.is_selected { "text-beet-leaf" } else { "text-white" },
          "{props.track.title}"
        }
        if let Some(duration) = &props.track.duration {
          span {
            class: "text-sm tabular-nums",
            class: if props.is_selected { "text-beet-leaf/80" } else { "text-muted" },
            "{duration}"
          }
        }
      }
    }
}
