use std::future::Future;
use std::pin::Pin;

use dioxus::prelude::*;

use crate::{FieldLook, Logo, TextInput};

type LoginCallback = Callback<(String, String), Pin<Box<dyn Future<Output = Result<(), String>>>>>;

#[derive(Props, PartialEq, Clone)]
pub struct Props {
    login: LoginCallback,
}

#[component]
pub fn Login(props: Props) -> Element {
    let mut username = use_signal(|| "".to_string());
    let mut password = use_signal(|| "".to_string());
    let mut error = use_signal(|| "".to_string());

    let handle_login = move || {
        let user = username.read().to_string();
        let pass = password.read().to_string();
        spawn(async move {
            error.set("".to_string());
            match props.login.call((user, pass)).await {
                Ok(_) => {
                    // Login success logic usually handled by parent/router redirect
                }
                Err(e) => error.set(e),
            }
        });
    };

    rsx! {
      div { class: "flex flex-col items-center justify-center min-h-screen text-white bg-gradient-to-b from-beet-dim to-black px-4",
        // bg decorations

        div { class: "p-8 md:p-12 bg-beet-dark rounded-xl shadow-2xl shadow-black w-full max-w-md relative z-10",
          // Header
          div { class: "flex flex-col items-center mb-8",
            Logo {
              size: 64,
              class: "mb-4",
            }
            h1 { class: "text-3xl font-extrabold tracking-tight text-white",
              "Soulbeet"
            }
            p { class: "text-xs text-muted font-semibold mt-2 tracking-widest",
              "YOUR LIBRARY MANAGER"
            }
          }

          // Form
          form {
            class: "space-y-6",
            onsubmit: move |e| {
                e.prevent_default();
                handle_login();
            },
            div {
              label { class: "block text-sm font-bold text-white mb-2",
                "Username"
              }
              TextInput {
                look: FieldLook::Login,
                value: username(),
                oninput: move |v| username.set(v),
                placeholder: "Enter username",
              }
            }
            div {
              label { class: "block text-sm font-bold text-white mb-2",
                "Password"
              }
              TextInput {
                look: FieldLook::Login,
                value: password(),
                oninput: move |v| password.set(v),
                placeholder: "Enter password",
                password: true,
              }
            }

            if !error().is_empty() {
              div { class: "p-3 bg-red-600 rounded-md text-white text-sm flex items-center gap-2",
                svg {
                  class: "w-4 h-4",
                  fill: "none",
                  view_box: "0 0 24 24",
                  stroke: "currentColor",
                  path {
                    stroke_linecap: "round",
                    stroke_linejoin: "round",
                    stroke_width: "2",
                    d: "M12 8v4m0 4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z",
                  }
                }
                "{error}"
              }
            }

            button {
              class: "w-full retro-btn py-3.5 text-sm flex justify-center items-center gap-2 group",
              "type": "submit",
              span { "AUTHENTICATE" }
              svg {
                class: "w-4 h-4 group-hover:translate-x-1 transition-transform",
                fill: "none",
                view_box: "0 0 24 24",
                stroke: "currentColor",
                path {
                  stroke_linecap: "round",
                  stroke_linejoin: "round",
                  stroke_width: "2",
                  d: "M14 5l7 7m0 0l-7 7m7-7H3",
                }
              }
            }
          }
        }
      }
    }
}
