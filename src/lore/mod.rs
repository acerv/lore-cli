pub mod atom;
pub mod mbox;
pub mod status;

use anyhow::{bail, Context, Result};
use reqwest::Client;

use crate::cache::Cache;
use crate::config::LoreConfig;
use crate::model::{Email, PatchEntry};

const USER_AGENT: &str = concat!("lore-cli/", env!("CARGO_PKG_VERSION"));

/// HTTP client for a specific lore/public-inbox server and project.
#[derive(Clone)]
pub struct LoreClient {
    http: Client,
    server: String,
    project: String,
    cache: Cache,
}

impl LoreClient {
    pub fn new(config: &LoreConfig) -> Result<Self> {
        let http = Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .context("building HTTP client")?;
        Ok(Self {
            http,
            server: config.server.clone(),
            project: config.project.clone(),
            cache: Cache::new(&config.server, &config.project),
        })
    }

    /// URL of a page of the patch-root Atom feed (newest first).
    ///
    /// `NOT s:Re:` drops replies so only thread roots remain; `rt:..` is a
    /// no-op filter the server requires; `o=` paginates in steps of 200.
    fn patch_list_url(&self, offset: usize) -> String {
        format!(
            "{}/{}/?x=A&q=rt:..+AND+NOT+s:Re:&o={}",
            self.server, self.project, offset
        )
    }

    /// URL of the gzipped mbox containing a whole thread.
    pub fn thread_mbox_url(&self, message_id: &str) -> String {
        format!("{}/{}/{}/t.mbox.gz", self.server, self.project, message_id)
    }

    /// Fetch and parse a page of patch roots.
    pub async fn fetch_patch_list(&self, offset: usize) -> Result<Vec<PatchEntry>> {
        let url = self.patch_list_url(offset);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("requesting {url}"))?;
        if !resp.status().is_success() {
            bail!("server returned {} for {}", resp.status(), url);
        }
        let bytes = resp.bytes().await.context("reading atom feed body")?;
        atom::parse_patch_list(&bytes, &self.project)
    }

    /// Fetch and parse the whole thread for a patch (served from cache if known).
    pub async fn fetch_thread(&self, message_id: &str) -> Result<Vec<Email>> {
        let raw = self.fetch_thread_raw(message_id).await?;
        Ok(mbox::parse_mbox(&raw))
    }

    /// Return the decompressed thread mbox, using the on-disk cache when present.
    async fn fetch_thread_raw(&self, message_id: &str) -> Result<Vec<u8>> {
        if let Some(data) = self.cache.get(message_id) {
            return Ok(data);
        }
        let url = self.thread_mbox_url(message_id);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("requesting {url}"))?;
        if !resp.status().is_success() {
            bail!("server returned {} for {}", resp.status(), url);
        }
        let bytes = resp.bytes().await.context("reading thread mbox body")?;
        let raw = mbox::gunzip(&bytes);
        self.cache.put(message_id, &raw);
        Ok(raw)
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;
    use crate::config::LoreConfig;

    /// End-to-end check against the real server. Run with:
    ///   cargo test -- --ignored live_pipeline
    #[tokio::test]
    #[ignore = "hits lore.kernel.org over the network"]
    async fn live_pipeline() {
        let cfg = LoreConfig {
            server: "https://lore.kernel.org".into(),
            project: "amd-gfx".into(),
        };
        let client = LoreClient::new(&cfg).unwrap();

        let patches = client.fetch_patch_list(0).await.unwrap();
        assert!(!patches.is_empty(), "expected some patches");

        let emails = client.fetch_thread(&patches[0].message_id).await.unwrap();
        assert!(!emails.is_empty(), "expected some emails in the thread");
        let _status = crate::lore::status::compute_status(&emails);

        // Second fetch should be served from the on-disk cache.
        let cached = client.fetch_thread(&patches[0].message_id).await.unwrap();
        assert_eq!(emails.len(), cached.len());
        eprintln!(
            "live_pipeline: {} patches, first thread {} emails, status {:?}",
            patches.len(),
            emails.len(),
            _status
        );
    }
}
