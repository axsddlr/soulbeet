//! One artist: follow controls, monitoring settings and the discography.

use std::collections::{HashMap, HashSet};

use api::models::folder::Folder;
use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use shared::artists::{ArtistPage as ArtistPageData, ArtistReleaseView, FollowSettings};
use ui::artists::{
    artist_meta, can_download, check_summary, download_outcome, group_by_kind, kind_label,
    kind_section_title, relative_time, release_year, CardGridSkeleton, InfoChip, ReleaseCover,
    StatusChip,
};
use ui::{friendly_error, use_auth, FIELD_CLASS};

use super::artists::{ArtistAvatar, ManualSearchLink, FOCUS_RING, SMALL_PRIMARY};

/// How often, and how many times, the page re-reads itself while the
/// baseline check that follows a follow is still running.
const BASELINE_POLL_MS: u32 = 3_000;
const BASELINE_POLLS: u32 = 10;

#[component]
pub fn ArtistPage(mbid: String) -> Element {
    let auth = use_auth();
    let mut current = use_signal(|| mbid.clone());
    let mut page = use_signal(|| None::<Result<ArtistPageData, String>>);
    let mut generation = use_signal(|| 0u64);
    let mut refreshing = use_signal(|| false);
    let mut folders = use_signal(Vec::<Folder>::new);
    let mut follow_busy = use_signal(|| false);
    let mut action_error = use_signal(|| None::<String>);
    let mut check_message = use_signal(|| None::<String>);
    let mut checking = use_signal(|| false);
    let busy = use_signal(HashSet::<String>::new);
    let messages = use_signal(HashMap::<String, (String, bool)>::new);

    // Reads the page; while a fresh follow's baseline check is still running
    // it reads again a few times, so statuses show up without a reload.
    let mut load = move || {
        let run = *generation.peek() + 1;
        generation.set(run);
        spawn(async move {
            refreshing.set(true);
            let mut polls = 0;
            loop {
                let mbid = current.peek().clone();
                let result = auth.call(api::get_artist_page(mbid)).await;
                if *generation.peek() != run {
                    return;
                }
                let pending = matches!(
                    &result,
                    Ok(p) if p.follow.is_some() && p.last_checked_at.is_none()
                );
                page.set(Some(result.map_err(|e| friendly_error(&e))));
                refreshing.set(false);
                if !pending || polls >= BASELINE_POLLS {
                    return;
                }
                polls += 1;
                TimeoutFuture::new(BASELINE_POLL_MS).await;
                if *generation.peek() != run {
                    return;
                }
            }
        });
    };

    use_effect(use_reactive((&mbid,), move |(mbid,)| {
        current.set(mbid);
        page.set(None);
        check_message.set(None);
        action_error.set(None);
        load();
    }));

    use_effect(move || {
        spawn(async move {
            if let Ok(list) = auth.call(api::get_user_folders()).await {
                folders.set(list);
            }
        });
    });

    let mut toggle_follow = move |(following, name): (bool, String)| {
        follow_busy.set(true);
        action_error.set(None);
        spawn(async move {
            let mbid = current.peek().clone();
            let result = if following {
                auth.call(api::unfollow_artist(mbid)).await
            } else {
                auth.call(api::follow_artist(mbid, name)).await.map(|_| ())
            };
            if let Err(e) = result {
                action_error.set(Some(friendly_error(&e)));
            }
            follow_busy.set(false);
            check_message.set(None);
            load();
        });
    };

    let save_settings = move |settings: FollowSettings| {
        action_error.set(None);
        spawn(async move {
            let mbid = current.peek().clone();
            if let Err(e) = auth.call(api::update_follow(mbid, settings)).await {
                action_error.set(Some(friendly_error(&e)));
            }
            load();
        });
    };

    let check_now = move |_| {
        checking.set(true);
        action_error.set(None);
        spawn(async move {
            let mbid = current.peek().clone();
            let text = match auth.call(api::check_followed_artist(mbid)).await {
                Ok(result) => check_summary(&result),
                Err(e) => friendly_error(&e),
            };
            checking.set(false);
            check_message.set(Some(text.clone()));
            load();
            TimeoutFuture::new(6_000).await;
            if check_message.peek().as_deref() == Some(text.as_str()) {
                check_message.set(None);
            }
        });
    };

    let data = match &*page.read() {
        None => return rsx! { ArtistSkeleton {} },
        Some(Err(message)) => {
            let message = message.clone();
            return rsx! {
                div { class: "flex flex-col items-center text-center gap-4 py-16 text-white",
                    p { class: "text-muted", "data-testid": "artist-error", "{message}" }
                    button {
                        class: "pill-outline {FOCUS_RING}",
                        onclick: move |_| load(),
                        "Try again"
                    }
                }
            };
        }
        Some(Ok(data)) => data.clone(),
    };

    let following = data.follow.is_some();
    let meta = artist_meta(&data.artist);
    let name = data.artist.name.clone();
    let baseline_pending = following && data.last_checked_at.is_none();
    let checked = data
        .last_checked_at
        .as_deref()
        .map(|ts| relative_time(ts, chrono::Utc::now()));
    let sections: Vec<_> = group_by_kind(&data.releases)
        .into_iter()
        .map(|(kind, releases)| (kind_label(kind), kind_section_title(kind), releases))
        .collect();
    let follow_class = if following {
        format!("pill-outline border-beet-accent! text-beet-leaf! {FOCUS_RING}")
    } else {
        format!("retro-btn {FOCUS_RING}")
    };

    rsx! {
        div { class: "w-full max-w-6xl mt-2 md:mt-6 text-white space-y-8",
            // Header
            div { class: "flex flex-col sm:flex-row items-center sm:items-end gap-4 md:gap-6",
                ArtistAvatar {
                    name: name.clone(),
                    class: "w-32 h-32 md:w-48 md:h-48 text-5xl md:text-7xl",
                }
                div { class: "min-w-0 w-full sm:flex-1 text-center sm:text-left",
                    p { class: "text-sm font-bold text-white mb-1", "Artist" }
                    h1 {
                        class: "text-3xl md:text-5xl lg:text-6xl font-extrabold tracking-tight text-white leading-tight break-words line-clamp-2",
                        "data-testid": "artist-name",
                        "{name}"
                    }
                    if !meta.is_empty() {
                        p { class: "mt-2 text-sm text-muted truncate", "{meta}" }
                    }
                }
            }

            // Actions
            div { class: "space-y-3",
                div { class: "flex flex-wrap items-center gap-3",
                    button {
                        class: "{follow_class}",
                        "data-testid": "follow-button",
                        "aria-pressed": "{following}",
                        disabled: follow_busy(),
                        onclick: {
                            let name = name.clone();
                            move |_| toggle_follow((following, name.clone()))
                        },
                        if following { "Following" } else { "Follow" }
                    }
                    if following {
                        button {
                            class: "pill-outline disabled:opacity-60 {FOCUS_RING}",
                            "data-testid": "check-now",
                            disabled: checking(),
                            onclick: check_now,
                            if checking() { "Checking…" } else { "Check now" }
                        }
                    }
                    button {
                        class: "p-2 rounded-full text-muted hover:text-white hover:bg-white/10 transition-colors cursor-pointer {FOCUS_RING}",
                        "aria-label": "Refresh",
                        title: "Refresh",
                        "data-testid": "artist-refresh",
                        onclick: move |_| load(),
                        svg {
                            class: if refreshing() { "w-5 h-5 animate-spin" } else { "w-5 h-5" },
                            fill: "none",
                            stroke: "currentColor",
                            stroke_width: "2",
                            view_box: "0 0 24 24",
                            "aria-hidden": "true",
                            path {
                                stroke_linecap: "round",
                                stroke_linejoin: "round",
                                d: "M16.023 9.348h4.992v-.001M2.985 19.644v-4.992m0 0h4.992m-4.993 0l3.181 3.183a8.25 8.25 0 0013.803-3.7M4.031 9.865a8.25 8.25 0 0113.803-3.7l3.181 3.182m0-4.991v4.99",
                            }
                        }
                    }
                }
                if let Some(text) = check_message() {
                    p { class: "text-sm text-beet-leaf", role: "status", "data-testid": "check-result",
                        "{text}"
                    }
                }
                if let Some(error) = action_error() {
                    p { class: "text-sm text-red-400", role: "alert", "{error}" }
                }
                if baseline_pending {
                    p { class: "text-sm text-muted", "data-testid": "baseline-pending",
                        "Checking this artist's releases. Statuses appear when the check finishes."
                    }
                } else if let Some(error) = data.last_error.clone() {
                    p { class: "text-sm text-red-400", "Last check failed: {error}" }
                } else if let Some(checked) = checked {
                    p { class: "text-xs text-muted", "Last checked {checked}" }
                }

                if let Some(settings) = data.follow.clone() {
                    FollowSettingsPanel {
                        settings,
                        folders: folders(),
                        on_change: save_settings,
                    }
                }
            }

            // Discography
            if sections.is_empty() {
                p { class: "text-muted py-8", "No albums, EPs or singles on MusicBrainz yet." }
            }
            for (label, heading, releases) in sections {
                section {
                    key: "{label}",
                    class: "space-y-4",
                    "data-testid": "discography-section",
                    "data-kind": "{label}",
                    h2 { class: "text-2xl font-bold text-white tracking-tight", "{heading}" }
                    ul { class: "list-none p-0 grid grid-cols-2 sm:grid-cols-3 md:grid-cols-[repeat(auto-fill,minmax(180px,1fr))] gap-4",
                        for release in releases {
                            ReleaseCard {
                                key: "{release.release_group_mbid}",
                                release,
                                artist_name: name.clone(),
                                following,
                                busy,
                                messages,
                                on_done: move |_| load(),
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn ReleaseCard(
    release: ArtistReleaseView,
    artist_name: String,
    following: bool,
    busy: Signal<HashSet<String>>,
    messages: Signal<HashMap<String, (String, bool)>>,
    on_done: EventHandler<()>,
) -> Element {
    let auth = use_auth();
    let rg = release.release_group_mbid.clone();
    let year = release_year(release.first_release_date.as_deref());
    let kind = kind_label(release.kind);
    let is_busy = busy.read().contains(&rg);
    let message = messages.read().get(&rg).cloned();
    let downloadable = following && can_download(release.status.as_deref());
    let tooltip = if !following {
        "Follow to download"
    } else if release.status.is_none() {
        "Not tracked yet. Refresh after the check finishes."
    } else if downloadable {
        "Download"
    } else {
        "Nothing to download right now"
    };

    let download = {
        let rg = rg.clone();
        move |_| {
            let rg = rg.clone();
            let mut busy = busy;
            let mut messages = messages;
            busy.write().insert(rg.clone());
            spawn(async move {
                let outcome = match auth.call(api::download_release(rg.clone())).await {
                    Ok(result) => download_outcome(&result),
                    Err(e) => (friendly_error(&e), true),
                };
                messages.write().insert(rg.clone(), outcome);
                busy.write().remove(&rg);
                on_done.call(());
            });
        }
    };

    rsx! {
        li {
            class: "h-full rounded-md bg-beet-panel hover:bg-beet-dim transition-colors duration-300 p-3 md:p-4 flex flex-col",
            "data-testid": "release-card",
            "data-rg": "{rg}",
            ReleaseCover {
                rg_mbid: rg.clone(),
                title: release.title.clone(),
                class: "w-full aspect-square rounded-md shadow-lg shadow-black/50 mb-3 md:mb-4",
            }
            p { class: "text-sm md:text-base font-bold text-white truncate", title: "{release.title}",
                "{release.title}"
            }
            p { class: "text-xs md:text-sm text-muted truncate mt-1",
                if let Some(year) = year {
                    "{year} · "
                }
                "{kind}"
            }
            div { class: "mt-2 flex flex-wrap items-center gap-1.5 min-h-5 min-w-0",
                StatusChip {
                    status: release.status.clone(),
                    detail: release.status_detail.clone(),
                }
                if release.in_library {
                    span { "data-testid": "in-library-chip",
                        InfoChip { label: "In library".to_string(), accent: true }
                    }
                }
            }
            if release.status.as_deref() == Some("manual") {
                div { class: "mt-2",
                    ManualSearchLink { artist: artist_name.clone(), title: release.title.clone() }
                }
            }
            if let Some((text, is_error)) = message {
                p {
                    class: if is_error { "mt-2 text-xs text-red-400" } else { "mt-2 text-xs text-beet-leaf" },
                    role: "status",
                    "data-testid": "release-message",
                    "{text}"
                }
            }
            div { class: "mt-auto pt-3",
                span { class: "inline-block", title: "{tooltip}",
                    button {
                        class: "{SMALL_PRIMARY} {FOCUS_RING}",
                        "data-testid": "release-download",
                        disabled: !downloadable || is_busy,
                        onclick: download,
                        if is_busy { "Downloading…" } else { "Download" }
                    }
                }
            }
        }
    }
}

#[component]
fn FollowSettingsPanel(
    settings: FollowSettings,
    folders: Vec<Folder>,
    on_change: EventHandler<FollowSettings>,
) -> Element {
    let singles = settings.include_singles;
    let auto = settings.auto_download;
    let folder = settings.folder_id.clone().unwrap_or_default();
    let for_singles = settings.clone();
    let for_auto = settings.clone();
    let for_folder = settings.clone();

    rsx! {
        div {
            class: "flex flex-col md:flex-row md:items-center gap-4 md:gap-8 p-4 rounded-lg bg-beet-panel",
            "data-testid": "follow-settings",
            Switch {
                label: "Include singles",
                checked: singles,
                testid: "setting-include-singles",
                on_toggle: move |value| {
                    on_change.call(FollowSettings { include_singles: value, ..for_singles.clone() })
                },
            }
            Switch {
                label: "Auto-download new releases",
                checked: auto,
                testid: "setting-auto-download",
                on_toggle: move |value| {
                    on_change.call(FollowSettings { auto_download: value, ..for_auto.clone() })
                },
            }
            label { class: "flex items-center gap-3 text-sm text-white min-w-0",
                span { class: "shrink-0", "Folder" }
                select {
                    class: "{FIELD_CLASS} md:w-56 text-sm",
                    "data-testid": "setting-folder",
                    value: "{folder}",
                    onchange: move |e| {
                        let value = e.value();
                        let folder_id = (!value.is_empty()).then_some(value);
                        on_change.call(FollowSettings { folder_id, ..for_folder.clone() })
                    },
                    option { value: "", selected: folder.is_empty(), "Default folder" }
                    for f in folders.iter() {
                        option {
                            key: "{f.id}",
                            value: "{f.id}",
                            selected: f.id == folder,
                            "{f.name}"
                        }
                    }
                }
            }
        }
    }
}

/// A labelled on/off switch.
#[component]
fn Switch(
    label: &'static str,
    checked: bool,
    testid: &'static str,
    on_toggle: EventHandler<bool>,
) -> Element {
    let track = if checked {
        "bg-beet-accent"
    } else {
        "bg-white/20"
    };
    let knob = if checked {
        "translate-x-4"
    } else {
        "translate-x-0"
    };
    rsx! {
        button {
            r#type: "button",
            role: "switch",
            "aria-checked": "{checked}",
            "data-testid": testid,
            class: "flex items-center gap-3 text-sm text-white cursor-pointer rounded-full {FOCUS_RING}",
            onclick: move |_| on_toggle.call(!checked),
            span { class: "relative inline-flex h-5 w-9 shrink-0 rounded-full transition-colors {track}",
                span { class: "absolute top-0.5 left-0.5 h-4 w-4 rounded-full bg-white shadow transition-transform {knob}" }
            }
            span { "{label}" }
        }
    }
}

/// The page's shape while it loads.
#[component]
fn ArtistSkeleton() -> Element {
    rsx! {
        div { class: "w-full max-w-6xl mt-2 md:mt-6 space-y-8", "aria-busy": "true",
            div { class: "flex flex-col sm:flex-row items-center sm:items-end gap-4 md:gap-6",
                div { class: "w-32 h-32 md:w-48 md:h-48 rounded-full bg-white/5 animate-pulse shrink-0" }
                div { class: "w-full sm:flex-1 space-y-3 flex flex-col items-center sm:items-start",
                    div { class: "h-4 w-16 rounded bg-white/5 animate-pulse" }
                    div { class: "h-10 md:h-14 w-2/3 max-w-md rounded bg-white/5 animate-pulse" }
                    div { class: "h-4 w-40 rounded bg-white/5 animate-pulse" }
                }
            }
            div { class: "h-11 w-32 rounded-full bg-white/5 animate-pulse" }
            div { class: "space-y-4",
                div { class: "h-7 w-28 rounded bg-white/5 animate-pulse" }
                CardGridSkeleton { cards: 6 }
            }
        }
    }
}
