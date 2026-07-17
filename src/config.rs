use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// Top-level configuration, parsed from `config.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub lore: LoreConfig,
    #[serde(default)]
    pub ui: UiConfig,
}

/// Which lore/public-inbox server and project (mailing list) to browse.
#[derive(Debug, Clone, Deserialize)]
pub struct LoreConfig {
    /// Base URL of the server, e.g. `https://lore.kernel.org`.
    pub server: String,
    /// Mailing list / inbox name, e.g. `amd-gfx`.
    pub project: String,
}

/// UI and fetching tunables.
#[derive(Debug, Clone, Deserialize)]
pub struct UiConfig {
    #[serde(default = "default_page_size")]
    pub page_size: usize,
    #[serde(default = "default_status_concurrency")]
    pub status_concurrency: usize,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            page_size: default_page_size(),
            status_concurrency: default_status_concurrency(),
        }
    }
}

fn default_page_size() -> usize {
    200
}

fn default_status_concurrency() -> usize {
    6
}

impl Config {
    /// Load and validate a configuration file.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config file {}", path.display()))?;
        let mut config: Config = toml::from_str(&text)
            .with_context(|| format!("parsing config file {}", path.display()))?;
        config.normalize()?;
        Ok(config)
    }

    /// Trim/clamp values and reject clearly invalid ones.
    fn normalize(&mut self) -> Result<()> {
        while self.lore.server.ends_with('/') {
            self.lore.server.pop();
        }
        if self.lore.server.is_empty() {
            bail!("lore.server must not be empty");
        }
        self.lore.project = self.lore.project.trim_matches('/').to_string();
        if self.lore.project.is_empty() {
            bail!("lore.project must not be empty");
        }
        if self.ui.page_size == 0 || self.ui.page_size > 200 {
            self.ui.page_size = default_page_size();
        }
        if self.ui.status_concurrency == 0 {
            self.ui.status_concurrency = default_status_concurrency();
        }
        Ok(())
    }
}
