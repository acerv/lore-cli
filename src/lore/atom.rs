use anyhow::{Context, Result};
use feed_rs::parser;

use crate::model::{PatchEntry, PatchStatus};

/// Parse a lore Atom feed into patch entries.
///
/// `project` is used to locate the Message-ID token inside each entry's link
/// (the path segment right after `/<project>/`).
pub fn parse_patch_list(bytes: &[u8], project: &str) -> Result<Vec<PatchEntry>> {
    let feed = parser::parse(bytes).context("parsing atom feed")?;
    let marker = format!("/{project}/");

    let mut entries = Vec::with_capacity(feed.entries.len());
    for entry in feed.entries {
        let message_id = entry
            .links
            .iter()
            .find_map(|link| extract_message_id(&link.href, &marker));
        let Some(message_id) = message_id else {
            continue;
        };

        let subject = entry.title.map(|t| clean_subject(&t.content)).unwrap_or_default();
        let (author_name, author_email) = entry
            .authors
            .into_iter()
            .next()
            .map(|person| (person.name, person.email.unwrap_or_default()))
            .unwrap_or_default();

        entries.push(PatchEntry {
            subject,
            author_name,
            author_email,
            message_id,
            updated: entry.updated.or(entry.published),
            status: PatchStatus::Unknown,
        });
    }
    Ok(entries)
}

/// Extract the Message-ID token from a link like
/// `https://host/<project>/<message-id>/` given `marker == "/<project>/"`.
fn extract_message_id(href: &str, marker: &str) -> Option<String> {
    let pos = href.find(marker)?;
    let rest = &href[pos + marker.len()..];
    let token = rest.split('/').next().unwrap_or("");
    (!token.is_empty()).then(|| token.to_string())
}

fn clean_subject(subject: &str) -> String {
    subject.split_whitespace().collect::<Vec<_>>().join(" ")
}
