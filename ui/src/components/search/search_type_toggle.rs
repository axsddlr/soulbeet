use dioxus::prelude::*;

pub use crate::settings_context::SearchType;

#[component]
pub fn SearchTypeToggle(search_type: Signal<SearchType>) -> Element {
    let active_class = "text-black bg-white";
    let inactive_class = "text-white bg-white/10 hover:bg-white/20";

    let album_class = if search_type() == SearchType::Album {
        active_class
    } else {
        inactive_class
    };
    let track_class = if search_type() == SearchType::Track {
        active_class
    } else {
        inactive_class
    };

    rsx! {
      div { class: "flex items-center gap-1.5 mr-2",
        button {
          class: "px-3 py-1.5 text-xs font-bold rounded-full transition-all duration-200 cursor-pointer {album_class}",
          onclick: move |_| search_type.set(SearchType::Album),
          "ALBUM"
        }
        button {
          class: "px-3 py-1.5 text-xs font-bold rounded-full transition-all duration-200 cursor-pointer {track_class}",
          onclick: move |_| search_type.set(SearchType::Track),
          "TRACK"
        }
      }
    }
}
