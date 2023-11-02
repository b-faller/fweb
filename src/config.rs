use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Error;

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

impl Config {
    /// Read and parse site config
    pub async fn from_file(path: impl AsRef<Path>) -> Result<Self, Error> {
        // Read and parse config.
        let path = path.as_ref();
        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| Error::ConfigRead(path.into(), e))?;
        let mut config: Config =
            toml::from_str(&content).map_err(|e| Error::ConfigParse(path.into(), e))?;

        // Make config paths relative to the configuration file.
        let basedir = path
            .parent()
            .expect("file does exist and must have a parent");
        config.content_path = basedir.join(&config.content_path);
        config.output_path = basedir.join(&config.output_path);

        Ok(config)
    }
}
