use std::path::PathBuf;

use thiserror::Error as ErrorTrait;

#[derive(Debug, ErrorTrait)]
pub enum Error {
    #[error("Source directory {0} is invalid: {1}")]
    SourceDir(PathBuf, std::io::Error),

    #[error("Reading config file {0} failed: {1}")]
    ConfigRead(PathBuf, std::io::Error),

    #[error("Content from file {0} has malformed frontmatter")]
    MalformedContent(PathBuf),

    #[error("Reading input file {0} failed: {1}")]
    ReadInput(PathBuf, std::io::Error),

    #[error("Wrinting file {0} failed: {1}")]
    WriteFile(PathBuf, std::io::Error),

    #[error("Parsing metadata from frontmatter failed for {0}: {1}")]
    ParseMetadata(PathBuf, toml::de::Error),

    #[error("Reading directory {0} failed: {1}")]
    ReadDirectory(PathBuf, std::io::Error),

    #[error("Create directory {0} failed: {1}")]
    CreateDirectory(PathBuf, std::io::Error),

    #[error("Failed to join page futures: {0}")]
    PageJoin(tokio::task::JoinError),

    #[error("Copying file {0} to {1} failed: {2}")]
    Copy(PathBuf, PathBuf, std::io::Error),

    #[error("Could not parse shortcode '{0}'")]
    ParseShortcode(String),

    #[error("Could include file {0}: {1}")]
    IncludeShortcode(PathBuf, std::io::Error),
}

/// Wrapper around the [Error]
pub type Result<T> = std::result::Result<T, Error>;
