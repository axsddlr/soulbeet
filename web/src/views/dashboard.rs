use dioxus::prelude::*;
use ui::dashboard::{DashboardTab, DashboardTabs, DeletionHistoryTab, StatsOverview};
use ui::discovery::DiscoveryOverview;

#[component]
pub fn DashboardPage() -> Element {
    let mut active_tab = use_signal(DashboardTab::default);

    rsx! {

        div { class: "space-y-6 text-white w-full max-w-6xl mt-2 md:mt-6",
            div { class: "mb-2",
                h1 { class: "text-3xl md:text-5xl font-extrabold tracking-tight text-white mb-2",
                    "Dashboard"
                }
            }

            div { class: "flex",
                DashboardTabs {
                    active: active_tab(),
                    on_change: move |tab| active_tab.set(tab),
                }
            }

            div { class: "pt-6",
                match active_tab() {
                    DashboardTab::Overview => rsx! { OverviewTab {} },
                    DashboardTab::History => rsx! { DeletionHistoryTab {} },
                    DashboardTab::Discovery => rsx! { DiscoveryOverview {} },
                }
            }
        }
    }
}

#[component]
fn OverviewTab() -> Element {
    let stats_resource = use_resource(|| async { api::get_library_stats().await });
    let binding = stats_resource.read();

    match &*binding {
        Some(Ok(s)) => {
            let s = s.clone();
            rsx! { StatsOverview { stats: s } }
        }
        Some(Err(_)) => rsx! {
            div { class: "text-center text-muted",
                "Failed to load library stats. Is Navidrome configured?"
            }
        },
        None => rsx! {
            div { class: "text-center text-muted animate-pulse",
                "Loading stats..."
            }
        },
    }
}
