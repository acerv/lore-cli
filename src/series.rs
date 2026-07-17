use std::collections::HashMap;

/// A grouping of the patch list: a standalone patch (no children) or a
/// patch-set whose cover letter (`0/N`) is the head and whose member patches
/// are the children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// Index (into the input) of the root row: a standalone patch or a cover.
    pub head: usize,
    /// Indices of the patch-set members (empty for a standalone patch), ordered
    /// by patch number.
    pub children: Vec<usize>,
}

/// Parse the `k/N` numbers from a subject's leading `[...]` tags.
pub fn parse_part(subject: &str) -> Option<(u32, u32)> {
    let mut rest = subject.trim();
    while let Some(inner) = rest.strip_prefix('[') {
        let Some(close) = inner.find(']') else {
            break;
        };
        for token in inner[..close].split(|c: char| c.is_whitespace() || c == ',') {
            if let Some((k, n)) = token.split_once('/') {
                if let (Ok(k), Ok(n)) = (k.parse::<u32>(), n.parse::<u32>()) {
                    return Some((k, n));
                }
            }
        }
        rest = inner[close + 1..].trim_start();
    }
    None
}

/// Derive a series key from a message-id by collapsing the git-send-email
/// sequence number (`...-<seq>-...`), so every message of one series matches.
fn series_stem(message_id: &str) -> String {
    let chars: Vec<char> = message_id.chars().collect();
    let mut found: Option<(usize, usize)> = None;
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '-' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 1 && j < chars.len() && chars[j] == '-' {
                found = Some((i, j)); // collapse chars[i..=j] into a single '-'
                i = j;
                continue;
            }
        }
        i += 1;
    }
    match found {
        Some((start, end)) => {
            let mut key: String = chars[..start].iter().collect();
            key.push('-');
            key.extend(&chars[end + 1..]);
            key
        }
        None => message_id.to_string(),
    }
}

/// Group `(subject, message_id)` pairs into standalone patches and patch-sets.
///
/// A cover letter (`0/N`) becomes a patch-set head; its members are the other
/// messages sharing the same series stem with a part number `>= 1`. Everything
/// else (standalone patches, or series members whose cover isn't present) stays
/// a top-level root. Groups are ordered by their newest (lowest-index) member.
pub fn group<'a>(items: impl IntoIterator<Item = (&'a str, &'a str)>) -> Vec<Group> {
    let entries: Vec<(Option<(u32, u32)>, String)> = items
        .into_iter()
        .map(|(subject, message_id)| (parse_part(subject), series_stem(message_id)))
        .collect();

    let mut by_stem: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut cover: HashMap<&str, usize> = HashMap::new();
    for (i, (part, stem)) in entries.iter().enumerate() {
        by_stem.entry(stem.as_str()).or_default().push(i);
        if matches!(part, Some((0, _))) {
            cover.entry(stem.as_str()).or_insert(i);
        }
    }

    let mut consumed = vec![false; entries.len()];
    let mut groups: Vec<Group> = Vec::new();

    // Each cover letter becomes a patch-set with its members as children.
    for (i, (part, stem)) in entries.iter().enumerate() {
        if matches!(part, Some((0, _))) && cover.get(stem.as_str()) == Some(&i) {
            let mut children: Vec<usize> = by_stem[stem.as_str()]
                .iter()
                .copied()
                .filter(|&j| j != i && matches!(entries[j].0, Some((k, _)) if k >= 1))
                .collect();
            children.sort_by_key(|&j| entries[j].0.map(|(k, _)| k).unwrap_or(0));
            consumed[i] = true;
            for &j in &children {
                consumed[j] = true;
            }
            groups.push(Group { head: i, children });
        }
    }

    // Everything not pulled into a patch-set is a standalone root.
    for (i, &done) in consumed.iter().enumerate() {
        if !done {
            groups.push(Group { head: i, children: Vec::new() });
        }
    }

    // Order each group by its newest (lowest-index) member.
    groups.sort_by_key(|g| {
        g.children
            .iter()
            .copied()
            .chain(std::iter::once(g.head))
            .min()
            .unwrap_or(g.head)
    });
    groups
}

/// Parse the version (`vN`, default 1) and a normalized title key of a subject.
fn version_of(subject: &str) -> (u32, String) {
    let mut rest = subject.trim();
    let mut version = 1;
    let mut found = false;
    while let Some(inner) = rest.strip_prefix('[') {
        let Some(close) = inner.find(']') else {
            break;
        };
        if !found {
            if let Some(v) = inner[..close]
                .split(|c: char| c.is_whitespace() || c == ',')
                .find_map(|t| {
                    let digits = t.strip_prefix('v').or_else(|| t.strip_prefix('V'))?;
                    digits.parse::<u32>().ok()
                })
            {
                version = v;
                found = true;
            }
        }
        rest = inner[close + 1..].trim_start();
    }
    let title = rest.trim();
    let key = if title.is_empty() { subject.trim() } else { title };
    (
        version,
        key.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase(),
    )
}

/// For each subject, whether a newer version of the same patch exists.
pub fn superseded_flags<'a>(subjects: impl IntoIterator<Item = &'a str>) -> Vec<bool> {
    let parsed: Vec<(u32, String)> = subjects.into_iter().map(version_of).collect();
    let mut max: HashMap<&str, u32> = HashMap::new();
    for (version, key) in &parsed {
        let entry = max.entry(key.as_str()).or_insert(0);
        *entry = (*entry).max(*version);
    }
    parsed
        .iter()
        .map(|(version, key)| *version < max[key.as_str()])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_part_reads_k_of_n() {
        assert_eq!(parse_part("[PATCH 0/3] cover"), Some((0, 3)));
        assert_eq!(parse_part("[PATCH 2/3] foo"), Some((2, 3)));
        assert_eq!(parse_part("[LTP] [PATCH v2 1/2] bar"), Some((1, 2)));
        assert_eq!(parse_part("[PATCH] standalone"), None);
        assert_eq!(parse_part("No tags at all"), None);
    }

    #[test]
    fn series_stem_collapses_sequence_number() {
        assert_eq!(
            series_stem("20260716.368565-1-a@amd.com"),
            series_stem("20260716.368565-3-a@amd.com")
        );
        assert_ne!(series_stem("aaa-1-a@x"), series_stem("bbb-1-a@x"));
        assert_eq!(series_stem("no-sequence@x"), "no-sequence@x");
    }

    #[test]
    fn cover_becomes_patchset_with_members() {
        let groups = group([
            ("[PATCH 2/2] two", "msg-3-a@x"),
            ("[PATCH 1/2] one", "msg-2-a@x"),
            ("[PATCH 0/2] cover", "msg-1-a@x"),
        ]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].head, 2); // the cover
        assert_eq!(groups[0].children, vec![1, 0]); // 1/2 then 2/2
    }

    #[test]
    fn standalone_patches_are_separate() {
        let groups = group([
            ("[PATCH] a fix", "x-1-a@x"),
            ("[PATCH] b fix", "y-1-b@x"),
        ]);
        assert_eq!(groups.len(), 2);
        assert!(groups.iter().all(|g| g.children.is_empty()));
    }

    #[test]
    fn members_without_a_cover_stay_separate() {
        let groups = group([
            ("[PATCH 1/2] one", "msg-1-a@x"),
            ("[PATCH 2/2] two", "msg-2-a@x"),
        ]);
        assert_eq!(groups.len(), 2);
        assert!(groups.iter().all(|g| g.children.is_empty()));
    }

    #[test]
    fn patchset_sorts_to_its_newest_member() {
        // newest-first: 2/2, unrelated, 1/2, cover
        let groups = group([
            ("[PATCH 2/2] two", "msg-3-a@x"),
            ("[PATCH] unrelated", "u-1-z@x"),
            ("[PATCH 1/2] one", "msg-2-a@x"),
            ("[PATCH 0/2] cover", "msg-1-a@x"),
        ]);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].head, 3); // cover heads the set
        assert_eq!(groups[0].children, vec![2, 0]);
        assert_eq!(groups[1].head, 1); // unrelated standalone
    }

    #[test]
    fn superseded_flags_mark_older_versions() {
        let flags = superseded_flags([
            "[PATCH v2] mm: fix",
            "[PATCH] mm: fix",
            "[PATCH] net: unrelated",
        ]);
        assert_eq!(flags, vec![false, true, false]);
    }

    #[test]
    fn superseded_flags_handle_list_tag_and_parts() {
        let flags = superseded_flags([
            "[LTP] [PATCH v3 1/2] a: x",
            "[LTP] [PATCH 1/2] a: x",
            "[LTP] [PATCH v2 1/2] a: x",
        ]);
        assert_eq!(flags, vec![false, true, true]); // v3 latest
    }
}
