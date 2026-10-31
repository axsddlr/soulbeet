use dioxus::prelude::*;

use crate::Logo;

/// The left sidebar on md and up: the logo, then the links and actions
/// passed as children. Put an item in a `mt-auto` wrapper to pin it to the
/// bottom.
#[component]
pub fn Navbar(children: Element) -> Element {
    rsx! {
        aside { class: "hidden md:flex w-60 shrink-0 flex-col bg-black rounded-lg px-3 py-5",
            // Logo area
            div { class: "flex items-center gap-2 px-3 mb-6 cursor-default",
                Logo { size: 32 }
                h1 { class: "text-xl font-bold tracking-tight text-white", "Soulbeet" }
            }

            // Menu
            nav { class: "flex flex-1 flex-col gap-1",
                {children}
            }
        }
    }
}
