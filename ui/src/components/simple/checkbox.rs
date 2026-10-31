use dioxus::prelude::*;

#[derive(Clone, PartialEq, Props)]
pub struct Props {
    is_selected: bool,
}

#[component]
pub fn Checkbox(props: Props) -> Element {
    rsx! {
      div {
        class: "w-4 h-4 border rounded-sm flex items-center justify-center transition-colors duration-200",
        class: if props.is_selected { "border-beet-accent bg-beet-accent text-black" } else { "border-muted bg-transparent" },
        if props.is_selected {
          svg {
            class: "w-3 h-3",
            fill: "none",
            stroke: "currentColor",
            view_box: "0 0 24 24",
            path {
              stroke_linecap: "round",
              stroke_linejoin: "round",
              stroke_width: "4",
              d: "M5 13l4 4L19 7",
            }
          }
        }
      }
    }
}
