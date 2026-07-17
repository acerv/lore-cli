use std::collections::HashMap;

/// A version group derived from patch subjects: the newest version is the head,
/// older versions are its children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub key: String,
    /// Index (into the input list) of the newest version.
    pub head: usize,
    /// Indices of older versions, newest-first.
    pub children: Vec<usize>,
}

/// Parse a patch subject into its `(version, series-key)`.
///
/// The version comes from a `vN` token inside the leading `[...]` tag (default
/// 1); the key is the normalized title after the tag, so different versions of
/// the same patch share a key.
pub fn parse(subject: &str) -> (u32, String) {
    let s = subject.trim();
    if let Some(rest) = s.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            let tag = &rest[..end];
            let title = rest[end + 1..].trim();
            if !title.is_empty() {
                let version = tag
                    .split(|c: char| c.is_whitespace() || c == ',')
                    .find_map(parse_version_token)
                    .unwrap_or(1);
                return (version, normalize(title));
            }
        }
    }
    (1, normalize(s))
}

fn parse_version_token(token: &str) -> Option<u32> {
    let digits = token.strip_prefix('v').or_else(|| token.strip_prefix('V'))?;
    digits.parse::<u32>().ok()
}

fn normalize(title: &str) -> String {
    title.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Group subjects into version trees.
///
/// Patches sharing a key are nested **only** when they span more than one
/// version (so unrelated same-titled `v1` patches stay separate). Groups are
/// ordered by their head's position in the input (newest-first upstream).
pub fn group<'a>(subjects: impl IntoIterator<Item = &'a str>) -> Vec<Group> {
    let mut order: Vec<String> = Vec::new();
    let mut members: HashMap<String, Vec<(usize, u32)>> = HashMap::new();

    for (index, subject) in subjects.into_iter().enumerate() {
        let (version, key) = parse(subject);
        members
            .entry(key.clone())
            .or_insert_with(|| {
                order.push(key.clone());
                Vec::new()
            })
            .push((index, version));
    }

    let mut groups: Vec<Group> = Vec::new();
    for key in order {
        let mut list = members.remove(&key).unwrap_or_default();
        let distinct_versions = {
            let mut versions: Vec<u32> = list.iter().map(|&(_, v)| v).collect();
            versions.sort_unstable();
            versions.dedup();
            versions.len()
        };

        if distinct_versions > 1 {
            // Newest version first; ties resolved by earliest position.
            list.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            let head = list[0].0;
            let children = list[1..].iter().map(|&(i, _)| i).collect();
            groups.push(Group { key, head, children });
        } else {
            // Single version throughout: not a version relationship.
            for (index, _) in list {
                groups.push(Group {
                    key: key.clone(),
                    head: index,
                    children: Vec::new(),
                });
            }
        }
    }

    groups.sort_by_key(|g| g.head);
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_extracts_version_and_key() {
        assert_eq!(parse("[PATCH] mm: fix"), (1, "mm: fix".into()));
        assert_eq!(parse("[PATCH v2] mm: fix"), (2, "mm: fix".into()));
        assert_eq!(parse("[PATCH v3 2/5] mm: fix foo"), (3, "mm: fix foo".into()));
        assert_eq!(parse("[RFC PATCH v2 19/20] drm: x"), (2, "drm: x".into()));
        assert_eq!(parse("[PATCH 1/2] mm: a"), (1, "mm: a".into()));
        assert_eq!(parse("No brackets here"), (1, "no brackets here".into()));
    }

    #[test]
    fn parse_ignores_case_and_whitespace_in_title() {
        assert_eq!(parse("[PATCH V4]   Mm:   Fix "), (4, "mm: fix".into()));
    }

    #[test]
    fn groups_versions_of_the_same_patch() {
        let groups = group(["[PATCH v2] mm: fix", "[PATCH] mm: fix"]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].head, 0); // v2 is the head
        assert_eq!(groups[0].children, vec![1]); // v1 nested
    }

    #[test]
    fn head_is_highest_version_regardless_of_position() {
        // Unusual order: v1, unrelated, v2.
        let groups = group([
            "[PATCH] mm: fix",
            "[PATCH] other: z",
            "[PATCH v2] mm: fix",
        ]);
        let mm = groups.iter().find(|g| g.key == "mm: fix").unwrap();
        assert_eq!(mm.head, 2);
        assert_eq!(mm.children, vec![0]);
    }

    #[test]
    fn same_version_duplicates_stay_separate() {
        let groups = group(["[PATCH] mm: fix", "[PATCH] mm: fix"]);
        assert_eq!(groups.len(), 2);
        assert!(groups.iter().all(|g| g.children.is_empty()));
    }

    #[test]
    fn unrelated_patches_are_not_grouped() {
        let groups = group(["[PATCH] a: x", "[PATCH v2] b: y"]);
        assert_eq!(groups.len(), 2);
        assert!(groups.iter().all(|g| g.children.is_empty()));
    }

    #[test]
    fn three_versions_nest_newest_first() {
        let groups = group([
            "[PATCH v3] mm: fix",
            "[PATCH v2] mm: fix",
            "[PATCH] mm: fix",
        ]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].head, 0); // v3
        assert_eq!(groups[0].children, vec![1, 2]); // v2 then v1
    }
}
