use dioxus::prelude::*;

/// The app shell: a full-height column holding the sidebar row, the
/// activity bar and, on phones, the tab bar.
#[component]
pub fn Layout(children: Element) -> Element {
    rsx! {
      div { class: "relative flex flex-col h-dvh w-full bg-black text-white overflow-hidden",
        {children}
      }
    }
}
