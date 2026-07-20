use std::collections::HashSet;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

/// File (within the cache dir) that records patches the user has marked as
/// viewed / not relevant, one Message-ID per line.
const MARKS_FILE: &str = "marked.txt";

/// On-disk cache of decompressed thread mboxes, keyed by Message-ID.
///
/// Caching keeps repeat runs fast and limits how often we hit the server while
/// determining patch status.
#[derive(Clone)]
pub struct Cache {
    dir: Option<PathBuf>,
}

impl Cache {
    /// Create a cache scoped to a server host and project.
    pub fn new(server: &str, project: &str) -> Self {
        let dir = directories::ProjectDirs::from("", "", "lore-cli").map(|dirs| {
            dirs.cache_dir()
                .join(sanitize(&host_of(server)))
                .join(sanitize(project))
        });
        if let Some(dir) = &dir {
            let _ = fs::create_dir_all(dir);
        }
        Self { dir }
    }

    /// A cache that touches no filesystem (used in tests for isolation).
    #[cfg(test)]
    pub fn disabled() -> Self {
        Self { dir: None }
    }

    pub fn get(&self, message_id: &str) -> Option<Vec<u8>> {
        let path = self.dir.as_ref()?.join(file_name(message_id));
        fs::read(path).ok()
    }

    pub fn put(&self, message_id: &str, data: &[u8]) {
        if let Some(dir) = &self.dir {
            let _ = fs::write(dir.join(file_name(message_id)), data);
        }
    }

    /// Load the set of Message-IDs the user marked as viewed / not relevant.
    /// Returns an empty set when no marks file exists yet.
    pub fn load_marks(&self) -> HashSet<String> {
        let Some(dir) = &self.dir else {
            return HashSet::new();
        };
        fs::read_to_string(dir.join(MARKS_FILE))
            .map(|text| {
                text.lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Persist the set of marked Message-IDs (sorted for a stable file).
    pub fn save_marks(&self, marks: &HashSet<String>) {
        if let Some(dir) = &self.dir {
            let mut ids: Vec<&str> = marks.iter().map(String::as_str).collect();
            ids.sort_unstable();
            let _ = fs::write(dir.join(MARKS_FILE), ids.join("\n"));
        }
    }
}

fn host_of(server: &str) -> String {
    server
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or("")
        .to_string()
}

fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Build a filesystem-safe file name from a Message-ID (readable prefix + hash
/// to avoid collisions after sanitizing).
fn file_name(message_id: &str) -> String {
    let mut safe = sanitize(message_id);
    safe.truncate(80);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    message_id.hash(&mut hasher);
    format!("{safe}-{:016x}.mbox", hasher.finish())
}
