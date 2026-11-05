mod artist;
mod artists;
mod dashboard;
mod login;
mod search;
mod settings;

pub use artist::ArtistPage;
pub use artists::ArtistsPage;
pub(crate) use artists::NewReleaseBadge;
pub use dashboard::DashboardPage;
pub use login::LoginPage;
pub use search::SearchPage;
pub use settings::SettingsPage;
pub(crate) use settings::{SettingsTab, SettingsTabRequest};
