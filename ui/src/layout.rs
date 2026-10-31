use dioxus::prelude::*;

/// The app shell: a fixed full-height grid. Rows: the sidebar and content
/// panel, the activity bar slot and, on phones, the tab bar.
#[component]
pub fn Layout(children: Element) -> Element {
    rsx! {
      div { class: "relative grid grid-cols-[minmax(0,1fr)] grid-rows-[minmax(0,1fr)_auto_auto] h-dvh w-full bg-black text-white overflow-hidden",
        {children}
      }
    }
}
