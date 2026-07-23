use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// Top-level configuration, parsed from `config.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub lore: LoreConfig,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub status: StatusConfig,
    #[serde(default)]
    pub cache: CacheConfig,
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

/// How patch status is detected from a thread.
#[derive(Debug, Clone, Deserialize)]
pub struct StatusConfig {
    /// Case-insensitive regular expressions that mark a patch as merged (green).
    /// Subsystems phrase this differently (e.g. "Applied, thanks" or
    /// "(applied|merged) to"); accepts a single string or a list.
    #[serde(
        default = "default_merged_markers",
        alias = "merged_marker",
        deserialize_with = "string_or_vec"
    )]
    pub merged_markers: Vec<String>,
}

impl Default for StatusConfig {
    fn default() -> Self {
        Self {
            merged_markers: default_merged_markers(),
        }
    }
}

fn default_merged_markers() -> Vec<String> {
    vec!["Merged, thanks".to_string()]
}

/// On-disk thread-cache tunables.
#[derive(Debug, Clone, Deserialize)]
pub struct CacheConfig {
    /// How long (seconds) a cached thread mbox is trusted before it is
    /// re-fetched from the server. Mailing lists gain new patches/replies over
    /// time, so an unbounded cache would keep serving a stale thread. `0`
    /// disables expiry (cache forever).
    #[serde(default = "default_max_age_secs")]
    pub max_age_secs: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            max_age_secs: default_max_age_secs(),
        }
    }
}

fn default_max_age_secs() -> u64 {
    900 // 15 minutes
}

/// Accept either a single string or a list of strings.
fn string_or_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{Error, SeqAccess, Visitor};
    use std::fmt;

    struct StringOrVec;
    impl<'de> Visitor<'de> for StringOrVec {
        type Value = Vec<String>;

        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a string or a list of strings")
        }

        fn visit_str<E: Error>(self, value: &str) -> Result<Self::Value, E> {
            Ok(vec![value.to_string()])
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while let Some(item) = seq.next_element::<String>()? {
                out.push(item);
            }
            Ok(out)
        }
    }
    deserializer.deserialize_any(StringOrVec)
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
        self.status.merged_markers.retain(|m| !m.trim().is_empty());
        if self.status.merged_markers.is_empty() {
            self.status.merged_markers = default_merged_markers();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_config_with_defaults() {
        let toml = r#"
            [lore]
            server = "https://lore.kernel.org/"
            project = "amd-gfx"
        "#;
        let mut config: Config = toml::from_str(toml).unwrap();
        config.normalize().unwrap();

        assert_eq!(config.lore.server, "https://lore.kernel.org"); // trailing slash trimmed
        assert_eq!(config.lore.project, "amd-gfx");
        assert_eq!(config.ui.page_size, 200);
        assert_eq!(config.ui.status_concurrency, 6);
        assert_eq!(config.status.merged_markers, vec!["Merged, thanks".to_string()]);
    }

    #[test]
    fn parses_merged_markers_string_or_list() {
        let base = "[lore]\nserver = \"https://x\"\nproject = \"p\"\n[status]\n";

        let list: Config = toml::from_str(&format!(
            "{base}merged_markers = [\"Merged, thanks\", \"Applied, thanks\"]\n"
        ))
        .unwrap();
        assert_eq!(
            list.status.merged_markers,
            vec!["Merged, thanks".to_string(), "Applied, thanks".to_string()]
        );

        let single: Config =
            toml::from_str(&format!("{base}merged_markers = \"Applied, thanks\"\n")).unwrap();
        assert_eq!(single.status.merged_markers, vec!["Applied, thanks".to_string()]);

        // Backward-compatible singular key.
        let old: Config = toml::from_str(&format!("{base}merged_marker = \"Pushed\"\n")).unwrap();
        assert_eq!(old.status.merged_markers, vec!["Pushed".to_string()]);
    }

    #[test]
    fn cache_max_age_defaults_and_overrides() {
        let base = "[lore]\nserver = \"https://x\"\nproject = \"p\"\n";

        // Default when the section is omitted.
        let default: Config = toml::from_str(base).unwrap();
        assert_eq!(default.cache.max_age_secs, 900);

        // Explicit override, including 0 (cache forever).
        let overridden: Config =
            toml::from_str(&format!("{base}[cache]\nmax_age_secs = 0\n")).unwrap();
        assert_eq!(overridden.cache.max_age_secs, 0);
    }

    #[test]
    fn rejects_empty_project() {
        let toml = "[lore]\nserver = \"https://x\"\nproject = \"\"\n";
        let mut config: Config = toml::from_str(toml).unwrap();
        assert!(config.normalize().is_err());
    }
}
