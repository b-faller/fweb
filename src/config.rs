use serde::{Deserialize, Serialize};

/// Information concerning the site.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Site {
    /// Site title.
    pub title: String,
    /// Short site description.
    pub description: String,
}

/// Generation configuration and global information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub site: Site,
}
