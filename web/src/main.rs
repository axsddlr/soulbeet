use auth::{use_auth, AuthProvider};
use dioxus::prelude::*;
#[cfg(feature = "web")]
use shared::download::DownloadEvent;
use shared::download::DownloadProgress;
use shared::system::NavidromeStatus;
use std::collections::HashMap;

#[cfg(feature = "web")]
use dioxus::fullstack::WebSocketOptions;
#[cfg(feature = "web")]
use websocket::use_resilient_websocket;

use ui::{
    ActivityBar, AutoDownloadSignal, Downloads, HealthProvider, Layout, Navbar, SearchPrefill,
    SearchReset, SettingsProvider,
};
use views::{DashboardPage, LoginPage, SearchPage, SettingsPage, SettingsTab, SettingsTabRequest};

mod auth;
mod views;
#[cfg(feature = "web")]
mod websocket;

#[derive(Debug, Clone, Routable, PartialEq)]
#[rustfmt::skip]
pub enum Route {
    #[layout(AuthGuard)]
        #[route("/login")]
        LoginPage {},

        #[layout(WebNavbar)]
            #[route("/")]
            SearchPage {},
            #[route("/dashboard")]
            DashboardPage {},
            #[route("/settings")]
            SettingsPage {},
}

const FAVICON: Asset = asset!("/assets/favicon.ico");
const MAIN_CSS: Asset = asset!("/assets/tailwind.css");

fn main() {
    #[cfg(feature = "server")]
    {
        use tower_cookies::CookieManagerLayer;

        dioxus::serve(|| async move {
            // Start background cleanup task for user channels
            api::globals::start_channel_cleanup_task();
            // Check the beets config in the background; never awaited here.
            api::importer_health::start_startup_probe();

            Ok(dioxus::server::router(App).layer(CookieManagerLayer::new()))
        });
    }

    #[cfg(not(feature = "server"))]
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    rsx! {
        // Browsers that take SVG icons use the logo; the .ico (rendered from
        // it) covers the rest.
        document::Link { rel: "icon", href: FAVICON, sizes: "16x16 32x32 48x48 256x256" }
        document::Link { rel: "icon", href: ui::LOGO, r#type: "image/svg+xml" }
        document::Link { rel: "stylesheet", href: MAIN_CSS }
        document::Meta { name: "viewport", content: "width=device-width, initial-scale=1" }
        document::Title { "SoulBeet" }

        AuthProvider {
            SettingsProvider {
                HealthProvider {
                    Router::<Route> {}
                }
            }
        }
    }
}

#[component]
fn AuthGuard() -> Element {
    let auth = use_auth();
    let nav = use_navigator();
    let current = use_route::<Route>();

    use_effect(move || {
        let is_logged_in = auth.is_logged_in();

        // If not logged in AND we're not already on /login -> go to login
        if !is_logged_in && !matches!(current, Route::LoginPage {}) {
            nav.replace(Route::LoginPage {});
        }

        // If logged in and on /login -> go to home
        if is_logged_in && matches!(current, Route::LoginPage {}) {
            nav.replace(Route::SearchPage {});
        }
    });

    rsx! {
        Outlet::<Route> {}
    }
}

#[component]
fn WebNavbar() -> Element {
    let mut auth = use_auth();
    let mut downloads_open = use_signal(|| false);
    let mut search_reset = use_signal(|| 0);
    #[allow(unused_mut)] // mutated in websocket callback (web feature only)
    let mut downloads = use_signal::<HashMap<String, DownloadProgress>>(HashMap::new);

    let search_prefill = use_signal(|| None::<(String, String)>);
    use_context_provider(|| SearchReset(search_reset));
    use_context_provider(|| SearchPrefill(search_prefill));
    let settings_tab_request = use_signal(|| None::<SettingsTab>);
    use_context_provider(|| SettingsTabRequest(settings_tab_request));

    #[allow(unused_mut)]
    let mut auto_download_signal = use_signal(|| None::<shared::download::AutoDownloadEvent>);
    use_context_provider(|| AutoDownloadSignal(auto_download_signal));

    #[cfg(feature = "web")]
    use_resilient_websocket(
        || api::download_updates_ws(WebSocketOptions::new()),
        move |event: DownloadEvent| match event {
            DownloadEvent::Progress(data) => {
                let mut map = downloads.write();
                for file in data {
                    map.insert(file.item.clone(), file);
                }
            }
            DownloadEvent::Replaced {
                previous_item,
                entry,
            } => {
                let mut map = downloads.write();
                map.remove(&previous_item);
                map.insert(entry.item.clone(), entry);
            }
            DownloadEvent::AutoDownload(auto_event) => {
                auto_download_signal.set(Some(auto_event));
            }
        },
    );

    let logout = move |_| {
        spawn(async move {
            auth.logout().await;
        });
    };

    let has_downloads = !downloads.read().is_empty();

    rsx! {
        Layout {
            div { class: "flex min-h-0 md:gap-2 md:p-2",
                Navbar {
                    Link {
                        class: SIDEBAR_LINK,
                        active_class: SIDEBAR_ACTIVE,
                        to: Route::SearchPage {},
                        onclick: move |_| search_reset += 1,
                        IconSearch { class: "w-6 h-6 shrink-0" }
                        span { "Search" }
                    }
                    Link {
                        class: SIDEBAR_LINK,
                        active_class: SIDEBAR_ACTIVE,
                        to: Route::DashboardPage {},
                        IconDashboard { class: "w-6 h-6 shrink-0" }
                        span { "Dashboard" }
                    }
                    Link {
                        class: SIDEBAR_LINK,
                        active_class: SIDEBAR_ACTIVE,
                        to: Route::SettingsPage {},
                        IconSettings { class: "w-6 h-6 shrink-0" }
                        span { "Settings" }
                    }

                    // Separator
                    div { class: "mx-3 my-3 h-px bg-white/10" }

                    // Downloads Toggle
                    button {
                        class: "{SIDEBAR_LINK} w-full text-left cursor-pointer focus:outline-none",
                        aria_label: "Downloads",
                        onclick: move |_| downloads_open.set(!downloads_open()),
                        span { class: "relative shrink-0",
                            IconDownloads { class: "w-6 h-6" }
                            if has_downloads {
                                DownloadsDot {}
                            }
                        }
                        span { "Downloads" }
                    }

                    div { class: "mt-auto px-3 pt-4",
                        button {
                            class: "pill-outline w-full py-2",
                            onclick: logout,
                            "Logout"
                        }
                    }
                }

                // Content panel
                div { class: "relative flex-1 min-w-0 flex flex-col bg-beet-dark md:rounded-lg overflow-hidden",
                    // Phone header: the sidebar's logo and logout
                    header { class: "md:hidden shrink-0 flex items-center justify-between px-4 py-3 bg-beet-dark",
                        div { class: "flex items-center gap-2",
                            ui::Logo { size: 32 }
                            span { class: "text-lg font-bold tracking-tight", "Soulbeet" }
                        }
                        button {
                            class: "text-xs font-semibold text-muted hover:text-white cursor-pointer px-3 py-1.5 rounded-full border border-white/20",
                            onclick: logout,
                            "Logout"
                        }
                    }

                    main { class: "relative flex-1 min-h-0 overflow-y-auto",
                        div { class: "pointer-events-none absolute inset-x-0 top-0 h-80 bg-gradient-to-b from-white/[0.08] to-transparent" }
                        div { class: "relative flex flex-col min-h-full px-4 sm:px-6 lg:px-8 pt-4 md:pt-6",
                            ImporterBanner {}
                            NavidromeBanner {}
                            div { class: "flex-grow flex flex-col w-full py-4",
                                Outlet::<Route> {}
                            }
                            ui::Footer {}
                        }
                    }
                }
            }

            // Activity bar slot. Desktop keeps the row at a fixed height, so the
            // sidebar does not shrink when the bar appears.
            div { class: "relative z-30 min-w-0 md:h-[72px]",
                ActivityBar { downloads, on_open: move |_| downloads_open.set(true) }
            }

            // Phone tab bar
            nav { class: "md:hidden relative z-30 grid grid-cols-4 bg-black border-t border-white/5 pb-[env(safe-area-inset-bottom)]",
                Link {
                    class: TAB_LINK,
                    active_class: TAB_ACTIVE,
                    to: Route::SearchPage {},
                    onclick: move |_| search_reset += 1,
                    IconSearch { class: "w-6 h-6" }
                    span { "Search" }
                }
                Link {
                    class: TAB_LINK,
                    active_class: TAB_ACTIVE,
                    to: Route::DashboardPage {},
                    IconDashboard { class: "w-6 h-6" }
                    span { "Dashboard" }
                }
                Link {
                    class: TAB_LINK,
                    active_class: TAB_ACTIVE,
                    to: Route::SettingsPage {},
                    IconSettings { class: "w-6 h-6" }
                    span { "Settings" }
                }
                button {
                    class: "{TAB_LINK} cursor-pointer focus:outline-none",
                    aria_label: "Downloads",
                    onclick: move |_| downloads_open.set(!downloads_open()),
                    span { class: "relative",
                        IconDownloads { class: "w-6 h-6" }
                        if has_downloads {
                            DownloadsDot {}
                        }
                    }
                    span { "Downloads" }
                }
            }

            Downloads { is_open: downloads_open, downloads }
        }
    }
}

const SIDEBAR_LINK: &str = "flex items-center gap-4 px-3 py-2.5 rounded-md text-sm font-bold text-muted hover:text-white transition-colors";
const SIDEBAR_ACTIVE: &str = "text-white! bg-white/[0.07]";
const TAB_LINK: &str = "flex flex-col items-center justify-center gap-1 py-2 text-[11px] font-medium text-muted hover:text-white transition-colors";
const TAB_ACTIVE: &str = "text-white!";

/// Pulses on the downloads icon while the queue has rows.
#[component]
fn DownloadsDot() -> Element {
    rsx! {
        span { class: "absolute -top-0.5 -right-0.5 flex h-2.5 w-2.5",
            span { class: "animate-ping absolute inline-flex h-full w-full rounded-full bg-beet-accent opacity-75" }
            span { class: "relative inline-flex rounded-full h-2.5 w-2.5 bg-beet-accent" }
        }
    }
}

#[component]
fn IconSearch(class: &'static str) -> Element {
    rsx! {
        svg {
            class,
            fill: "none",
            stroke: "currentColor",
            view_box: "0 0 24 24",
            path {
                stroke_linecap: "round",
                stroke_linejoin: "round",
                stroke_width: "2",
                d: "M21 21l-6-6m2-5a7 7 0 11-14 0 7 7 0 0114 0z",
            }
        }
    }
}

#[component]
fn IconDashboard(class: &'static str) -> Element {
    rsx! {
        svg {
            class,
            fill: "none",
            stroke: "currentColor",
            view_box: "0 0 24 24",
            stroke_width: "1.5",
            path {
                stroke_linecap: "round",
                stroke_linejoin: "round",
                d: "M3 13.125C3 12.504 3.504 12 4.125 12h2.25c.621 0 1.125.504 1.125 1.125v6.75C7.5 20.496 6.996 21 6.375 21h-2.25A1.125 1.125 0 013 19.875v-6.75zM9.75 8.625c0-.621.504-1.125 1.125-1.125h2.25c.621 0 1.125.504 1.125 1.125v11.25c0 .621-.504 1.125-1.125 1.125h-2.25a1.125 1.125 0 01-1.125-1.125V8.625zM16.5 4.125c0-.621.504-1.125 1.125-1.125h2.25C20.496 3 21 3.504 21 4.125v15.75c0 .621-.504 1.125-1.125 1.125h-2.25a1.125 1.125 0 01-1.125-1.125V4.125z",
            }
        }
    }
}

#[component]
fn IconSettings(class: &'static str) -> Element {
    rsx! {
        svg {
            class,
            fill: "none",
            stroke: "currentColor",
            view_box: "0 0 24 24",
            path {
                stroke_linecap: "round",
                stroke_linejoin: "round",
                stroke_width: "2",
                d: "M10.325 4.317c.426-1.756 2.924-1.756 3.35 0a1.724 1.724 0 002.573 1.066c1.543-.94 3.31.826 2.37 2.37a1.724 1.724 0 001.065 2.572c1.756.426 1.756 2.924 0 3.35a1.724 1.724 0 00-1.066 2.573c.94 1.543-.826 3.31-2.37 2.37a1.724 1.724 0 00-2.572 1.065c-.426 1.756-2.924 1.756-3.35 0a1.724 1.724 0 00-2.573-1.066c-1.543.94-3.31-.826-2.37-2.37a1.724 1.724 0 00-1.065-2.572c-1.756-.426-1.756-2.924 0-3.35a1.724 1.724 0 001.066-2.573c-.94-1.543.826-3.31 2.37-2.37.996.608 2.296.07 2.572-1.065z",
            }
            path {
                stroke_linecap: "round",
                stroke_linejoin: "round",
                stroke_width: "2",
                d: "M15 12a3 3 0 11-6 0 3 3 0 016 0z",
            }
        }
    }
}

#[component]
fn IconDownloads(class: &'static str) -> Element {
    rsx! {
        svg {
            class,
            fill: "none",
            stroke: "currentColor",
            view_box: "0 0 24 24",
            path {
                stroke_linecap: "round",
                stroke_linejoin: "round",
                stroke_width: "2",
                d: "M4 16v1a3 3 0 003 3h10a3 3 0 003-3v-1m-4-4l-4 4m0 0l-4-4m4 4V4",
            }
        }
    }
}

/// Shown while beets cannot import. Not dismissible: downloads are refused until it is fixed.
#[component]
fn ImporterBanner() -> Element {
    let health = ui::use_system_health();
    let SettingsTabRequest(mut requested_tab) = use_context();

    let importer = health.importer();
    let Some(issue) = importer.blocking_issue() else {
        return rsx! {};
    };

    rsx! {
        StatusBanner {
            dot_class: ui::importer_chip(importer.level).0,
            label: "BEETS",
            message: issue.title.clone(),
            Link {
                class: "shrink-0 px-3 py-1 rounded-full bg-white text-black font-bold hover:scale-[1.03] transition-transform",
                to: Route::SettingsPage {},
                onclick: move |_| requested_tab.set(Some(SettingsTab::Config)),
                "Fix in Config"
            }
        }
    }
}

/// One line under the navbar: a status dot, a label (hidden on small
/// screens), a message and an action.
#[component]
fn StatusBanner(
    dot_class: &'static str,
    label: &'static str,
    message: String,
    children: Element,
) -> Element {
    rsx! {
        div { class: "mb-2 px-4 py-2.5 bg-beet-dim/80 rounded-lg flex items-center gap-3 text-xs",
            span { class: "w-1.5 h-1.5 rounded-full {dot_class} shrink-0" }
            span { class: "text-white font-bold uppercase tracking-wider shrink-0 hidden sm:inline", "{label}" }
            span { class: "text-muted flex-1 min-w-0 truncate", "{message}" }
            {children}
        }
    }
}

#[component]
fn NavidromeBanner() -> Element {
    let auth = use_auth();
    let mut settings = ui::use_settings();
    let health = ui::use_system_health();
    let status = auth.navidrome_status();

    let dismissed = settings
        .get()
        .map(|s| s.navidrome_banner_dismissed)
        .unwrap_or(false);

    // The status is a login-time snapshot; live health overrides it, so the
    // banner clears on its own once Navidrome answers again.
    let back_online = matches!(status, NavidromeStatus::Offline) && health.navidrome_online();

    if dismissed
        || back_online
        || matches!(
            status,
            NavidromeStatus::Connected | NavidromeStatus::Unknown
        )
    {
        return rsx! {};
    }

    let (dot_color, label, message) = match status {
        NavidromeStatus::InvalidCredentials => (
            "bg-red-500",
            "NAVIDROME",
            "Credentials mismatch. Log in with your Navidrome password to reconnect.",
        ),
        NavidromeStatus::Offline => (
            "bg-gray-500",
            "NAVIDROME",
            "Unreachable. Discovery and rating sync paused.",
        ),
        NavidromeStatus::MissingReportRealPath => (
            "bg-yellow-500",
            "NAVIDROME",
            "ReportRealPath is not enabled on the Soulbeet player. Path features and auto-delete won't work. Enable it in Navidrome > Settings > Players.",
        ),
        _ => return rsx! {},
    };

    let dismiss = move |_| {
        spawn(async move {
            let _ = settings
                .update(api::UpdateUserSettings {
                    navidrome_banner_dismissed: Some(true),
                    ..Default::default()
                })
                .await;
        });
    };

    rsx! {
        StatusBanner { dot_class: dot_color, label, message: message.to_string(),
            button {
                class: "shrink-0 text-muted/60 hover:text-white/80 transition-colors cursor-pointer p-2 -mr-1",
                onclick: dismiss,
                svg {
                    class: "w-3.5 h-3.5",
                    fill: "none",
                    stroke: "currentColor",
                    view_box: "0 0 24 24",
                    path {
                        stroke_linecap: "round",
                        stroke_linejoin: "round",
                        stroke_width: "2",
                        d: "M6 18L18 6M6 6l12 12",
                    }
                }
            }
        }
    }
}
