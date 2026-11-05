//! The artists page: artist search, new releases from followed artists and
//! the list of followed artists.

use std::collections::{HashMap, HashSet};

use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use shared::artists::{FollowedArtistView, NewReleaseView};
use shared::metadata::ArtistSummary;
use ui::artists::{
    artist_meta, can_dismiss, can_download, download_outcome, kind_and_date, new_count,
    relative_time, InfoChip, ReleaseCover, RowSkeleton, StatusChip,
};
use ui::{friendly_error, use_auth, SearchPrefill};

use crate::Route;

/// Releases from followed artists waiting on the user, for the nav badge.
/// The navbar fetches it once; the artists page keeps it current.
#[derive(Clone, Copy)]
pub(crate) struct NewReleaseBadge(pub Signal<usize>);

/// Keyboard focus ring shared by the artist screens' controls.
pub(crate) const FOCUS_RING: &str =
    "focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-white";

/// A small pill action, e.g. Download or Dismiss.
pub(crate) const SMALL_PRIMARY: &str = "retro-btn px-4! py-2! text-[11px]! disabled:opacity-50 disabled:cursor-not-allowed disabled:hover:scale-100 disabled:hover:bg-beet-accent";
pub(crate) const SMALL_SECONDARY: &str = "pill-outline px-4! py-2! text-[11px]! disabled:opacity-50 disabled:cursor-not-allowed disabled:hover:scale-100";

#[derive(Clone, Copy, PartialEq)]
enum ArtistsTab {
    NewReleases,
    Following,
}

#[component]
pub fn ArtistsPage() -> Element {
    let mut tab = use_signal(|| ArtistsTab::NewReleases);

    let tab_button = move |label: &'static str, value: ArtistsTab, testid: &'static str| {
        let active = tab() == value;
        let class = if active {
            "px-4 py-1.5 rounded-full bg-white text-black text-sm font-semibold cursor-pointer shrink-0"
        } else {
            "px-4 py-1.5 rounded-full bg-white/10 text-white text-sm font-semibold hover:bg-white/20 cursor-pointer shrink-0"
        };
        rsx! {
            button {
                class: "{class} {FOCUS_RING}",
                role: "tab",
                "aria-selected": "{active}",
                "data-testid": testid,
                onclick: move |_| tab.set(value),
                "{label}"
            }
        }
    };

    rsx! {
        div { class: "space-y-6 text-white w-full max-w-6xl mt-2 md:mt-6",
            div { class: "space-y-2",
                h1 { class: "text-3xl md:text-5xl font-extrabold tracking-tight text-white",
                    "Artists"
                }
                p { class: "text-muted text-sm",
                    "Follow artists to hear about their new releases."
                }
            }

            ArtistSearch {}

            nav {
                class: "flex items-center gap-2 overflow-x-auto no-scrollbar whitespace-nowrap",
                role: "tablist",
                {tab_button("New releases", ArtistsTab::NewReleases, "tab-new-releases")}
                {tab_button("Following", ArtistsTab::Following, "tab-following")}
            }

            div { class: "pt-2",
                match tab() {
                    ArtistsTab::NewReleases => rsx! { NewReleases {} },
                    ArtistsTab::Following => rsx! { FollowingList {} },
                }
            }
        }
    }
}

/// A debounced MusicBrainz artist search; results link to the artist page.
#[component]
fn ArtistSearch() -> Element {
    let auth = use_auth();
    let mut query = use_signal(String::new);
    let mut results = use_signal(|| None::<Result<Vec<ArtistSummary>, String>>);
    let mut loading = use_signal(|| false);
    let mut generation = use_signal(|| 0u64);

    let on_input = move |event: FormEvent| {
        let value = event.value();
        query.set(value.clone());
        let current = *generation.peek() + 1;
        generation.set(current);
        let term = value.trim().to_string();
        if term.is_empty() {
            results.set(None);
            loading.set(false);
            return;
        }
        loading.set(true);
        spawn(async move {
            TimeoutFuture::new(300).await;
            if *generation.peek() != current {
                return;
            }
            let found = auth.call(api::search_artists(term)).await;
            if *generation.peek() != current {
                return;
            }
            results.set(Some(found.map_err(|e| friendly_error(&e))));
            loading.set(false);
        });
    };

    rsx! {
        div { class: "w-full space-y-3",
            div { class: "flex items-center bg-beet-input rounded-full h-12 pr-4 ring-1 ring-transparent hover:ring-white/20 hover:bg-[#2a2a2a] focus-within:ring-2 focus-within:ring-white transition-all",
                div { class: "pl-5 pr-3 text-muted",
                    svg {
                        class: "w-5 h-5",
                        fill: "none",
                        stroke: "currentColor",
                        view_box: "0 0 24 24",
                        "aria-hidden": "true",
                        path {
                            stroke_linecap: "round",
                            stroke_linejoin: "round",
                            stroke_width: "2",
                            d: "M21 21l-6-6m2-5a7 7 0 11-14 0 7 7 0 0114 0z",
                        }
                    }
                }
                input {
                    r#type: "search",
                    value: "{query}",
                    class: "flex-1 min-w-0 bg-transparent border-none focus:ring-0 text-white text-base placeholder-muted h-10 focus:outline-none",
                    placeholder: "Search artists to follow...",
                    "aria-label": "Search artists",
                    "data-testid": "artist-search-input",
                    oninput: on_input,
                }
                if loading() {
                    div {
                        class: "w-4 h-4 rounded-full border-2 border-white/20 border-t-beet-accent animate-spin shrink-0",
                        "aria-label": "Searching",
                    }
                }
            }

            match &*results.read() {
                Some(Ok(list)) if list.is_empty() => rsx! {
                    p { class: "px-2 text-sm text-muted", "No artists found." }
                },
                Some(Ok(list)) => rsx! {
                    ul { class: "flex flex-col", "data-testid": "artist-results",
                        for (artist, meta) in list.iter().map(|a| (a, artist_meta(a))) {
                            li {
                                key: "{artist.mbid}",
                                "data-testid": "artist-result",
                                "data-mbid": "{artist.mbid}",
                                Link {
                                    class: "flex items-center gap-3 p-2 rounded-md hover:bg-white/[0.07] transition-colors {FOCUS_RING}",
                                    to: Route::ArtistPage { mbid: artist.mbid.clone() },
                                    ArtistAvatar { name: artist.name.clone(), class: "w-12 h-12 text-lg" }
                                    div { class: "min-w-0 flex-1",
                                        p { class: "text-sm md:text-base font-bold text-white truncate",
                                            "{artist.name}"
                                        }
                                        p { class: "text-xs md:text-sm text-muted truncate",
                                            if meta.is_empty() { "Artist" } else { "{meta}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                },
                Some(Err(message)) => rsx! {
                    p { class: "px-2 text-sm text-red-400", "{message}" }
                },
                None => rsx! {},
            }
        }
    }
}

/// A round placeholder with the artist's initial; MusicBrainz has no photos.
#[component]
pub(crate) fn ArtistAvatar(name: String, class: &'static str) -> Element {
    let initial = name
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".to_string());
    rsx! {
        div {
            class: "{class} shrink-0 rounded-full bg-gradient-to-br from-[#535353] to-[#282828] flex items-center justify-center font-extrabold text-white/80 shadow-lg shadow-black/40",
            "aria-hidden": "true",
            "{initial}"
        }
    }
}

/// Sends the search page off looking for a release by name.
#[component]
pub(crate) fn ManualSearchLink(artist: String, title: String) -> Element {
    let prefill = try_use_context::<SearchPrefill>();
    rsx! {
        Link {
            class: "text-xs font-bold text-beet-leaf hover:underline rounded {FOCUS_RING}",
            to: Route::SearchPage {},
            onclick: move |_| {
                if let Some(SearchPrefill(mut slot)) = prefill {
                    slot.set(Some((artist.clone(), title.clone())));
                }
            },
            span { "data-testid": "manual-search", "Search manually" }
        }
    }
}

#[component]
fn NewReleases() -> Element {
    let auth = use_auth();
    let badge = try_use_context::<NewReleaseBadge>();
    let mut releases = use_signal(|| None::<Result<Vec<NewReleaseView>, String>>);
    let mut busy = use_signal(HashSet::<String>::new);
    let mut messages = use_signal(HashMap::<String, (String, bool)>::new);

    let load = move || {
        spawn(async move {
            let result = auth.call(api::list_new_releases()).await;
            if let (Ok(list), Some(NewReleaseBadge(mut count))) = (&result, badge) {
                count.set(new_count(
                    list.iter().filter_map(|r| r.release.status.as_deref()),
                ));
            }
            releases.set(Some(result.map_err(|e| friendly_error(&e))));
        });
    };

    use_effect(move || {
        load();
    });

    let mut download = move |rg: String| {
        busy.write().insert(rg.clone());
        spawn(async move {
            let outcome = match auth.call(api::download_release(rg.clone())).await {
                Ok(result) => download_outcome(&result),
                Err(e) => (friendly_error(&e), true),
            };
            messages.write().insert(rg.clone(), outcome);
            busy.write().remove(&rg);
            load();
        });
    };

    let mut dismiss = move |rg: String| {
        busy.write().insert(rg.clone());
        spawn(async move {
            if let Err(e) = auth.call(api::dismiss_release(rg.clone())).await {
                messages
                    .write()
                    .insert(rg.clone(), (friendly_error(&e), true));
            } else {
                messages.write().remove(&rg);
            }
            busy.write().remove(&rg);
            load();
        });
    };

    let binding = releases.read();
    match &*binding {
        None => rsx! { RowSkeleton { rows: 4 } },
        Some(Err(message)) => rsx! {
            p { class: "text-center text-muted py-8", "{message}" }
        },
        Some(Ok(list)) if list.is_empty() => rsx! {
            div {
                class: "flex flex-col items-center text-center gap-4 py-12",
                "data-testid": "new-releases-empty",
                p { class: "text-muted", "No new releases yet. Follow artists to get notified." }
                button {
                    class: "retro-btn {FOCUS_RING}",
                    onclick: move |_| {
                        let _ = document::eval(
                            "document.querySelector('[data-testid=\"artist-search-input\"]')?.focus()",
                        );
                    },
                    "Find artists"
                }
            }
        },
        Some(Ok(list)) => rsx! {
            ul { class: "flex flex-col gap-1", "data-testid": "new-releases",
                for item in list.iter() {
                    {
                        let rg = item.release.release_group_mbid.clone();
                        let status = item.release.status.clone();
                        let is_busy = busy.read().contains(&rg);
                        let message = messages.read().get(&rg).cloned();
                        let rg_download = rg.clone();
                        let rg_dismiss = rg.clone();
                        let kind_line = kind_and_date(
                            item.release.kind,
                            item.release.first_release_date.as_deref(),
                        );
                        rsx! {
                            li {
                                key: "{rg}",
                                class: "flex flex-wrap sm:flex-nowrap items-center gap-x-3 gap-y-2 p-2 rounded-md hover:bg-white/[0.04] transition-colors",
                                "data-testid": "new-release-row",
                                "data-rg": "{rg}",
                                ReleaseCover {
                                    rg_mbid: rg.clone(),
                                    title: item.release.title.clone(),
                                    class: "w-14 h-14 rounded shadow-lg shadow-black/40",
                                }
                                div { class: "min-w-0 flex-1 basis-40",
                                    p { class: "text-sm md:text-base font-bold text-white truncate",
                                        "{item.release.title}"
                                    }
                                    p { class: "text-xs md:text-sm text-muted truncate",
                                        Link {
                                            class: "hover:underline hover:text-white rounded {FOCUS_RING}",
                                            to: Route::ArtistPage { mbid: item.artist_mbid.clone() },
                                            "{item.artist_name}"
                                        }
                                        " · {kind_line}"
                                    }
                                    div { class: "mt-1 flex flex-wrap items-center gap-2 min-w-0",
                                        StatusChip {
                                            status: status.clone(),
                                            detail: item.release.status_detail.clone(),
                                        }
                                        if status.as_deref() == Some("manual") {
                                            ManualSearchLink {
                                                artist: item.artist_name.clone(),
                                                title: item.release.title.clone(),
                                            }
                                        }
                                    }
                                    if let Some((text, is_error)) = message {
                                        p {
                                            class: if is_error { "mt-1 text-xs text-red-400" } else { "mt-1 text-xs text-beet-leaf" },
                                            "data-testid": "release-message",
                                            role: "status",
                                            "{text}"
                                        }
                                    }
                                }
                                div { class: "flex items-center gap-2 shrink-0 ml-auto",
                                    if can_download(status.as_deref()) {
                                        button {
                                            class: "{SMALL_PRIMARY} {FOCUS_RING}",
                                            "data-testid": "release-download",
                                            disabled: is_busy,
                                            onclick: move |_| download(rg_download.clone()),
                                            "Download"
                                        }
                                    }
                                    if can_dismiss(status.as_deref()) {
                                        button {
                                            class: "{SMALL_SECONDARY} {FOCUS_RING}",
                                            "data-testid": "release-dismiss",
                                            disabled: is_busy,
                                            onclick: move |_| dismiss(rg_dismiss.clone()),
                                            "Dismiss"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        },
    }
}

#[component]
fn FollowingList() -> Element {
    let auth = use_auth();
    let followed =
        use_resource(move || async move { auth.call(api::list_followed_artists()).await });
    let now = chrono::Utc::now();

    let binding = followed.read();
    match &*binding {
        None => rsx! { RowSkeleton { rows: 3 } },
        Some(Err(e)) => {
            let message = friendly_error(e);
            rsx! {
                p { class: "text-center text-muted py-8", "{message}" }
            }
        }
        Some(Ok(list)) if list.is_empty() => rsx! {
            p { class: "text-center text-muted py-12", "data-testid": "following-empty",
                "You are not following any artists yet."
            }
        },
        Some(Ok(list)) => rsx! {
            ul { class: "flex flex-col gap-1", "data-testid": "followed-artists",
                for artist in list.iter() {
                    FollowedRow { key: "{artist.artist_mbid}", artist: artist.clone(), now }
                }
            }
        },
    }
}

#[component]
fn FollowedRow(artist: FollowedArtistView, now: chrono::DateTime<chrono::Utc>) -> Element {
    let checked = artist
        .last_checked_at
        .as_deref()
        .map(|ts| relative_time(ts, now));
    let counts = format!(
        "{} new · {} upcoming",
        artist.new_count, artist.upcoming_count
    );
    rsx! {
        li {
            class: "flex flex-wrap sm:flex-nowrap items-center gap-x-3 gap-y-1 p-2 rounded-md hover:bg-white/[0.04] transition-colors",
            "data-testid": "followed-artist",
            "data-mbid": "{artist.artist_mbid}",
            ArtistAvatar { name: artist.name.clone(), class: "w-12 h-12 text-lg" }
            div { class: "min-w-0 flex-1 basis-40",
                Link {
                    class: "block text-sm md:text-base font-bold text-white truncate hover:underline rounded {FOCUS_RING}",
                    to: Route::ArtistPage { mbid: artist.artist_mbid.clone() },
                    "{artist.name}"
                }
                p { class: "text-xs md:text-sm text-muted truncate",
                    span { class: if artist.new_count > 0 { "text-beet-leaf font-bold" }, "{counts}" }
                    " · "
                    if let Some(error) = &artist.last_error {
                        span { class: "text-red-400", title: "{error}", "Check failed: {error}" }
                    } else if let Some(checked) = checked {
                        "Checked {checked}"
                    } else {
                        "Checking…"
                    }
                }
            }
            div { class: "shrink-0 ml-auto",
                InfoChip {
                    label: if artist.settings.auto_download { "Auto-download on".to_string() } else { "Auto-download off".to_string() },
                    accent: artist.settings.auto_download,
                }
            }
        }
    }
}
