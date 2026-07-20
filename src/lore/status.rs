use crate::model::{Email, PatchStatus};
use regex::{Regex, RegexBuilder};

/// Determine a patch's status from the emails of its thread.
///
/// A thread reply matching any `merged_markers` regex wins (green); otherwise a
/// `Reviewed-by:` trailer anywhere marks it reviewed (yellow); otherwise it is
/// normal. Quoted lines (starting with `>`) are ignored to avoid matching text
/// someone merely quoted back.
///
/// Each marker is a case-insensitive regular expression matched against every
/// non-quoted line. Invalid patterns are skipped (they never match).
pub fn compute_status(emails: &[Email], merged_markers: &[String]) -> PatchStatus {
    let markers = compile_markers(merged_markers);
    let mut reviewed = false;
    for email in emails {
        if body_has_merged(&email.body, &markers) {
            return PatchStatus::Merged;
        }
        if !reviewed && body_has_reviewed_by(&email.body) {
            reviewed = true;
        }
    }
    if reviewed {
        PatchStatus::Reviewed
    } else {
        PatchStatus::Normal
    }
}

/// Compile each marker string into a case-insensitive regex, skipping any that
/// fail to parse so a bad pattern can't break status detection.
fn compile_markers(merged_markers: &[String]) -> Vec<Regex> {
    merged_markers
        .iter()
        .filter_map(|m| RegexBuilder::new(m).case_insensitive(true).build().ok())
        .collect()
}

fn is_quoted(line: &str) -> bool {
    line.trim_start().starts_with('>')
}

fn body_has_merged(body: &str, markers: &[Regex]) -> bool {
    body.lines()
        .filter(|line| !is_quoted(line))
        .any(|line| markers.iter().any(|re| re.is_match(line)))
}

fn body_has_reviewed_by(body: &str) -> bool {
    body.lines()
        .filter(|line| !is_quoted(line))
        .any(|line| line.trim_start().to_ascii_lowercase().starts_with("reviewed-by:"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn email(body: &str) -> Email {
        Email {
            from: "Author <a@example.com>".into(),
            date: "Mon, 1 Jan 2024 00:00:00 +0000".into(),
            subject: "[PATCH] test".into(),
            message_id: "id@example.com".into(),
            in_reply_to: None,
            body: body.into(),
        }
    }

    fn status(emails: &[Email]) -> PatchStatus {
        compute_status(emails, &["Merged, thanks".to_string()])
    }

    #[test]
    fn normal_when_no_tags() {
        let emails = vec![email("Here is a patch.\n")];
        assert_eq!(status(&emails), PatchStatus::Normal);
    }

    #[test]
    fn reviewed_when_reply_has_trailer() {
        let emails = vec![email("Please review.\n"), email("Reviewed-by: Bob <b@x>\n")];
        assert_eq!(status(&emails), PatchStatus::Reviewed);
    }

    #[test]
    fn reviewed_when_trailer_in_root_email() {
        let emails = vec![email("commit message\n\nReviewed-by: Bob <b@x>\n\n---\n code\n")];
        assert_eq!(status(&emails), PatchStatus::Reviewed);
    }

    #[test]
    fn merged_wins_over_reviewed() {
        let emails = vec![email("Reviewed-by: Bob <b@x>\n"), email("Merged, thanks\n")];
        assert_eq!(status(&emails), PatchStatus::Merged);
    }

    #[test]
    fn merged_is_case_insensitive() {
        let emails = vec![email("Applied. merged, THANKS!\n")];
        assert_eq!(status(&emails), PatchStatus::Merged);
    }

    #[test]
    fn ignores_quoted_merged() {
        let emails = vec![email("> Merged, thanks\nActually not yet.\n")];
        assert_eq!(status(&emails), PatchStatus::Normal);
    }

    #[test]
    fn ignores_quoted_reviewed_by() {
        let emails = vec![email("> Reviewed-by: Bob <b@x>\nI disagree.\n")];
        assert_eq!(status(&emails), PatchStatus::Normal);
    }

    #[test]
    fn custom_marker_is_respected() {
        let emails = vec![email("Applied, thanks!\n")];
        assert_eq!(
            compute_status(&emails, &["Applied, thanks".to_string()]),
            PatchStatus::Merged
        );
        assert_eq!(
            compute_status(&emails, &["Merged, thanks".to_string()]),
            PatchStatus::Normal
        );
    }

    #[test]
    fn any_of_multiple_markers_matches() {
        let emails = vec![email("Applied, thanks!\n")];
        let markers = ["Merged, thanks".to_string(), "Applied, thanks".to_string()];
        assert_eq!(compute_status(&emails, &markers), PatchStatus::Merged);
    }

    #[test]
    fn marker_is_treated_as_regex() {
        let markers = [r"(applied|merged|pushed) to".to_string()];
        assert_eq!(
            compute_status(&[email("Pushed to for-next.\n")], &markers),
            PatchStatus::Merged
        );
        assert_eq!(
            compute_status(&[email("applied TO my-tree\n")], &markers),
            PatchStatus::Merged
        );
        assert_eq!(
            compute_status(&[email("I applied for a job.\n")], &markers),
            PatchStatus::Normal
        );
    }

    #[test]
    fn invalid_regex_marker_is_skipped() {
        // An unbalanced group is invalid; it should simply never match rather
        // than break detection for a valid sibling marker.
        let markers = ["(unclosed".to_string(), "Merged, thanks".to_string()];
        assert_eq!(
            compute_status(&[email("Merged, thanks!\n")], &markers),
            PatchStatus::Merged
        );
    }
}
