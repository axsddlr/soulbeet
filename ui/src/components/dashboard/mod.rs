use dioxus::prelude::*;
use shared::navidrome::LibraryStats;

#[derive(PartialEq, Clone, Copy, Default)]
pub enum DashboardTab {
    #[default]
    Overview,
    History,
    Discovery,
}

#[component]
pub fn DashboardTabs(active: DashboardTab, on_change: EventHandler<DashboardTab>) -> Element {
    let tab = move |label: &'static str, tab: DashboardTab| {
        let active_class = if active == tab {
            "px-4 py-1.5 rounded-full bg-white text-black text-sm font-semibold cursor-pointer shrink-0"
        } else {
            "px-4 py-1.5 rounded-full bg-white/10 text-white text-sm font-semibold hover:bg-white/20 cursor-pointer shrink-0"
        };
        rsx! {
            button {
                class: active_class,
                onclick: move |_| on_change.call(tab),
                "{label}"
            }
        }
    };

    rsx! {
        nav { class: "flex items-center gap-2 overflow-x-auto no-scrollbar whitespace-nowrap",
            {tab("Overview", DashboardTab::Overview)}
            {tab("History", DashboardTab::History)}
            {tab("Discovery", DashboardTab::Discovery)}
        }
    }
}

#[component]
pub fn StatsOverview(stats: LibraryStats) -> Element {
    let star_rows: Vec<(String, u32, u32)> = stats
        .rating_distribution
        .iter()
        .enumerate()
        .map(|(i, count)| {
            let stars = i + 1;
            let max = *stats.rating_distribution.iter().max().unwrap_or(&1).max(&1);
            let pct = (*count as f64 / max as f64 * 100.0) as u32;
            let star_label = if stars == 1 {
                "1 star".to_string()
            } else {
                format!("{stars} stars")
            };
            (star_label, *count, pct)
        })
        .collect();

    rsx! {
        div { class: "space-y-6",
            div { class: "grid grid-cols-2 md:grid-cols-4 gap-4",
                StatCard { label: "Total Tracks", value: format!("{}", stats.total_tracks) }
                StatCard { label: "Rated", value: format!("{}", stats.rated_tracks) }
                StatCard { label: "Unrated", value: format!("{}", stats.unrated_tracks) }
                StatCard { label: "Avg Rating", value: format!("{:.1}", stats.average_rating) }
                StatCard { label: "Albums", value: format!("{}", stats.total_albums) }
                StatCard { label: "Artists", value: format!("{}", stats.total_artists) }
            }

            div { class: "bg-beet-panel p-4 rounded-lg",
                h3 { class: "text-sm font-semibold text-white mb-3", "Rating Distribution" }
                div { class: "space-y-2",
                    for (star_label, count, pct) in &star_rows {
                        div { class: "flex items-center gap-3",
                            span { class: "text-xs text-muted w-14 shrink-0",
                                "{star_label}"
                            }
                            div { class: "flex-1 bg-white/10 rounded-full h-2 overflow-hidden",
                                div {
                                    class: "h-full bg-beet-accent rounded-full transition-all",
                                    style: "width: {pct}%",
                                }
                            }
                            span { class: "text-xs text-muted w-10 text-right shrink-0",
                                "{count}"
                            }
                        }
                    }
                }
            }

            if !stats.genres.is_empty() {
                div { class: "bg-beet-panel p-4 rounded-lg",
                    h3 { class: "text-sm font-semibold text-white mb-3", "Top Genres" }
                    div { class: "flex flex-wrap gap-2",
                        for (genre, count) in &stats.genres {
                            span { class: "px-3 py-1 bg-white/10 rounded-full text-xs font-medium text-white",
                                "{genre} ({count})"
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn StatCard(label: &'static str, value: String) -> Element {
    rsx! {
        div { class: "bg-beet-panel p-4 rounded-lg",
            p { class: "text-xs font-semibold text-muted mb-1", "{label}" }
            p { class: "text-2xl font-bold text-white", "{value}" }
        }
    }
}

#[component]
pub fn DeletionHistoryTab() -> Element {
    let history = use_resource(|| async { api::get_deletion_history().await });

    let items = match &*history.read() {
        Some(Ok(items)) => items.clone(),
        _ => vec![],
    };

    rsx! {
        div { class: "space-y-4",
            div { class: "flex items-center justify-between",
                h3 { class: "text-sm font-semibold text-white", "Deletion History" }
                p { class: "text-xs text-muted/80",
                    "Configure auto-delete in Settings > Library"
                }
            }

            if items.is_empty() {
                p { class: "text-muted/80 text-sm", "No deletion history yet." }
            } else {
                div { class: "space-y-1 max-h-96 overflow-y-auto",
                    for item in items {
                        div { class: "flex items-center justify-between p-2 bg-beet-panel rounded text-sm",
                            div { class: "flex-1 min-w-0",
                                span { class: "text-white truncate", "{item.title}" }
                                span { class: "text-muted mx-2", "-" }
                                span { class: "text-muted truncate", "{item.artist}" }
                            }
                            span { class: "text-xs ml-2 text-red-400", "Deleted" }
                        }
                    }
                }
            }
        }
    }
}
