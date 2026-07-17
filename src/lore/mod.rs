pub mod atom;
pub mod mbox;
pub mod status;

use anyhow::{bail, Context, Result};
use reqwest::Client;

use crate::config::LoreConfig;
use crate::model::{Email, PatchEntry};

const USER_AGENT: &str = concat!("lore-cli/", env!("CARGO_PKG_VERSION"));

/// HTTP client for a specific lore/public-inbox server and project.
#[derive(Clone)]
pub struct LoreClient {
    http: Client,
    server: String,
    project: String,
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

    /// Fetch and parse the whole thread for a patch.
    pub async fn fetch_thread(&self, message_id: &str) -> Result<Vec<Email>> {
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
        mbox::parse_thread_mbox(&bytes)
    }
}
