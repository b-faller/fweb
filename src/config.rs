use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Information concerning the site.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiteInfo {
    /// Site title.
    pub title: String,
    /// Short site description.
    pub description: String,
}

/// Generation configuration and global information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Information about the website
    pub site_info: SiteInfo,

    /// Directory with the website its source files.
    /// Relative to `config.toml`.
    #[serde(default = "default_content_path")]
    pub content_path: PathBuf,

    /// Directory where the built site is created at.
    /// Relative to `config.toml`.
    #[serde(default = "default_output_path")]
    pub output_path: PathBuf,
}

fn default_content_path() -> PathBuf {
    ".".into()
}

fn default_output_path() -> PathBuf {
    "_site".into()
}
