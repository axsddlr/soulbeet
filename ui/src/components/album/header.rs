use dioxus::prelude::*;
use shared::metadata::Album;

use crate::CoverArt;

#[derive(Props, PartialEq, Clone)]
pub struct Props {
    album: Album,
    /// Shown as "N tracks" after the artist and year when known.
    #[props(default)]
    track_count: Option<usize>,
}

/// A Spotify-style album header: large cover over a gradient, a small
/// "Album" label, the title and an artist · year · N tracks line.
#[component]
pub fn AlbumHeader(props: Props) -> Element {
    let year = props
        .album
        .release_date
        .as_deref()
        .map(|d| d.get(..4).unwrap_or(d).to_string());

    rsx! {
      div { class: "flex flex-col sm:flex-row sm:items-end gap-4 md:gap-6 p-4 md:p-6 bg-gradient-to-b from-beet-dim to-beet-panel",
        CoverArt {
          album: props.album.clone(),
          class: "w-40 h-40 md:w-[232px] md:h-[232px] rounded shadow-2xl shadow-black/60",
        }
        div { class: "min-w-0 flex-1",
          p { class: "text-xs font-bold text-white mb-1 md:mb-2", "Album" }
          h3 { class: "text-3xl md:text-5xl lg:text-7xl font-extrabold tracking-tight text-white leading-none break-words line-clamp-2 pb-1",
            "{props.album.title}"
          }
          p { class: "mt-3 md:mt-5 text-sm text-muted",
            span { class: "font-bold text-white", "{props.album.artist}" }
            if let Some(year) = year {
              " · {year}"
            }
            if let Some(n) = props.track_count {
              " · {n} tracks"
            }
          }
        }
      }
    }
}
