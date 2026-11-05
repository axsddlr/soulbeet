use dioxus::prelude::*;
use ui::Search;

use crate::Route;

#[component]
pub fn SearchPage() -> Element {
    rsx! {
        Search {
            actions: rsx! {
                Link {
                    class: "pill-outline px-4! py-2! gap-2 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-white",
                    to: Route::ArtistsPage {},
                    span { "data-testid": "find-artists", "Find artists" }
                }
            },
        }
    }
}
