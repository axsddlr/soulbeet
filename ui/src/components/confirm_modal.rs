use dioxus::prelude::*;

use crate::Modal;

#[derive(Props, PartialEq, Clone)]
pub struct ConfirmModalProps {
    pub message: String,
    #[props(default = "Confirm".to_string())]
    pub confirm_label: String,
    #[props(default = false)]
    pub danger: bool,
    pub on_confirm: EventHandler,
    pub on_cancel: EventHandler,
}

#[component]
pub fn ConfirmModal(props: ConfirmModalProps) -> Element {
    let confirm_class = if props.danger {
        "px-5 py-2 text-sm font-bold rounded-full bg-red-500 text-black hover:bg-red-400 hover:scale-[1.03] transition-all cursor-pointer"
    } else {
        "retro-btn text-sm"
    };

    rsx! {
        Modal {
            on_close: move |_| props.on_cancel.call(()),
            header: rsx! {
                h2 { class: "text-xl font-bold text-white", "Confirm" }
            },
            p { class: "text-muted text-sm mb-5", "{props.message}" }
            div { class: "flex justify-end gap-2",
                button {
                    class: "px-5 py-2 text-sm font-bold rounded-full text-muted hover:text-white hover:scale-[1.03] transition-all cursor-pointer",
                    onclick: move |_| props.on_cancel.call(()),
                    "Cancel"
                }
                button {
                    class: confirm_class,
                    onclick: move |_| props.on_confirm.call(()),
                    "{props.confirm_label}"
                }
            }
        }
    }
}
