use dioxus::prelude::*;

#[derive(Clone, PartialEq, Default)]
pub enum ButtonVariant {
    #[default]
    Primary,
    Secondary,
}

impl ButtonVariant {
    fn get_classes(&self) -> &'static str {
        match self {
            ButtonVariant::Primary => "retro-btn",
            // Outlined pill
            ButtonVariant::Secondary => "pill-outline",
        }
    }
}

#[derive(Props, Clone, PartialEq)]
pub struct Props {
    children: Element,
    #[props(into)]
    onclick: EventHandler<MouseEvent>,
    #[props(optional, default)]
    variant: ButtonVariant,
    #[props(optional, default)]
    disabled: bool,
    #[props(optional, into)]
    class: String,
}

#[component]
pub fn Button(props: Props) -> Element {
    let variant_classes = props.variant.get_classes();
    let disabled_classes = if props.disabled {
        "opacity-40 cursor-not-allowed pointer-events-none"
    } else {
        ""
    };
    let additional_classes = props.class;

    rsx! {
        button {
            class: "{variant_classes} {disabled_classes} {additional_classes}",
            onclick: move |evt| {
                if !props.disabled {
                    props.onclick.call(evt)
                }
            },
            disabled: props.disabled,
            {props.children}
        }
    }
}
