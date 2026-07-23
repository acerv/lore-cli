use std::collections::HashSet;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::Duration;

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
    /// How long a cached thread mbox is trusted before `get` treats it as a
    /// miss (forcing a re-fetch). `None` means cache forever.
    ttl: Option<Duration>,
}

impl Cache {
    /// Create a cache scoped to a server host and project.
    ///
    /// `max_age_secs` bounds how long a cached thread mbox is trusted; `0`
    /// disables expiry.
    pub fn new(server: &str, project: &str, max_age_secs: u64) -> Self {
        let dir = directories::ProjectDirs::from("", "", "lore-cli").map(|dirs| {
            dirs.cache_dir()
                .join(sanitize(&host_of(server)))
                .join(sanitize(project))
        });
        if let Some(dir) = &dir {
            let _ = fs::create_dir_all(dir);
        }
        Self {
            dir,
            ttl: (max_age_secs > 0).then(|| Duration::from_secs(max_age_secs)),
        }
    }

    /// A cache that touches no filesystem (used in tests for isolation).
    #[cfg(test)]
    pub fn disabled() -> Self {
        Self {
            dir: None,
            ttl: None,
        }
    }

    /// Return a cached thread mbox, or `None` when absent or expired.
    ///
    /// Expiry uses the file's modification time: a mailing-list thread can gain
    /// new patches after we first cached it, so anything older than the TTL is
    /// reported as a miss to force a fresh fetch.
    pub fn get(&self, message_id: &str) -> Option<Vec<u8>> {
        let path = self.dir.as_ref()?.join(file_name(message_id));
        if let Some(ttl) = self.ttl {
            let age = fs::metadata(&path)
                .and_then(|meta| meta.modified())
                .ok()?
                .elapsed()
                .unwrap_or_default();
            if age > ttl {
                return None;
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    /// Build a cache rooted at a unique temp dir with an explicit TTL.
    fn temp_cache(ttl: Option<Duration>) -> Cache {
        let dir = std::env::temp_dir().join(format!(
            "lore-cli-test-{}-{:?}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        Cache {
            dir: Some(dir),
            ttl,
        }
    }

    #[test]
    fn roundtrips_without_expiry() {
        let cache = temp_cache(None);
        cache.put("m1@example", b"hello");
        assert_eq!(cache.get("m1@example").as_deref(), Some(&b"hello"[..]));
    }

    #[test]
    fn fresh_entry_is_served_within_ttl() {
        let cache = temp_cache(Some(Duration::from_secs(60)));
        cache.put("m2@example", b"data");
        assert_eq!(cache.get("m2@example").as_deref(), Some(&b"data"[..]));
    }

    #[test]
    fn expired_entry_reads_as_miss() {
        // Zero-length TTL means anything already written is already too old.
        let cache = temp_cache(Some(Duration::ZERO));
        cache.put("m3@example", b"stale");
        // Ensure a nonzero age has elapsed since the write.
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(cache.get("m3@example"), None);
    }

    #[test]
    fn missing_entry_is_none() {
        let cache = temp_cache(Some(Duration::from_secs(60)));
        assert_eq!(cache.get("absent@example"), None);
    }
}
