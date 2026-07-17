use chrono::{DateTime, Utc};

/// Merge/review state of a patch, derived from its thread.
#[allow(dead_code)] // Reviewed/Merged are produced by status detection (later step).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PatchStatus {
    /// Not yet determined (thread not fetched).
    #[default]
    Unknown,
    /// Neither merged nor reviewed.
    Normal,
    /// Has a `Reviewed-by:` trailer but is not merged.
    Reviewed,
    /// A thread reply contains "Merged, thanks".
    Merged,
}

/// A patch (thread root) as listed for a project.
#[derive(Debug, Clone)]
pub struct PatchEntry {
    pub subject: String,
    pub author_name: String,
    pub author_email: String,
    /// Message-ID token exactly as it appears in the lore URL path
    /// (already percent-encoded); used to build thread URLs.
    pub message_id: String,
    pub updated: Option<DateTime<Utc>>,
    pub status: PatchStatus,
}
