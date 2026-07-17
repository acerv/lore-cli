use std::io::Read;

use flate2::read::GzDecoder;
use mailparse::{parse_mail, MailHeaderMap, ParsedMail};

use crate::model::Email;

/// Decompress gzip data, falling back to the input as-is when it is not gzipped.
pub fn gunzip(data: &[u8]) -> Vec<u8> {
    if data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b {
        let mut decoder = GzDecoder::new(data);
        let mut out = Vec::new();
        if decoder.read_to_end(&mut out).is_ok() {
            return out;
        }
    }
    data.to_vec()
}

/// Split an mboxrd document into messages and parse each one.
///
/// public-inbox uses mboxrd, where body lines starting with `From ` are escaped
/// with a leading `>`, so an unescaped `From ` at column 0 reliably marks a new
/// message.
pub fn parse_mbox(raw: &[u8]) -> Vec<Email> {
    let text = String::from_utf8_lossy(raw);
    let mut messages: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut started = false;

    for line in text.split_inclusive('\n') {
        if line.starts_with("From ") {
            if started && !current.is_empty() {
                messages.push(std::mem::take(&mut current));
            }
            started = true;
            continue; // drop the "From " separator line
        }
        if started {
            current.push_str(line);
        }
    }
    if !current.is_empty() {
        messages.push(current);
    }

    messages.iter().filter_map(|msg| parse_message(msg)).collect()
}

fn parse_message(raw: &str) -> Option<Email> {
    let parsed = parse_mail(raw.as_bytes()).ok()?;
    let headers = &parsed.headers;

    let message_id = headers
        .get_first_value("Message-ID")
        .or_else(|| headers.get_first_value("Message-Id"));

    Some(Email {
        from: headers.get_first_value("From").unwrap_or_default(),
        date: headers.get_first_value("Date").unwrap_or_default(),
        subject: headers.get_first_value("Subject").unwrap_or_default(),
        message_id: strip_angle_brackets(&message_id.unwrap_or_default()),
        in_reply_to: headers
            .get_first_value("In-Reply-To")
            .map(|v| strip_angle_brackets(&v)),
        body: unescape_mboxrd(&extract_body(&parsed)),
    })
}

/// Return the decoded text body, preferring the first `text/plain` part.
fn extract_body(parsed: &ParsedMail) -> String {
    if parsed.subparts.is_empty() {
        return parsed.get_body().unwrap_or_default();
    }
    find_text_plain(parsed).unwrap_or_default()
}

fn find_text_plain(part: &ParsedMail) -> Option<String> {
    if part.subparts.is_empty() {
        if part.ctype.mimetype == "text/plain" {
            return part.get_body().ok();
        }
        return None;
    }
    part.subparts.iter().find_map(find_text_plain)
}

/// Undo mboxrd escaping: a line matching `^>+From ` loses one leading `>`.
fn unescape_mboxrd(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    for line in body.split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']);
        let stripped = content.trim_start_matches('>');
        if stripped.len() < content.len() && stripped.starts_with("From ") {
            out.push_str(&line[1..]);
        } else {
            out.push_str(line);
        }
    }
    out
}

fn strip_angle_brackets(value: &str) -> String {
    value.trim().trim_start_matches('<').trim_end_matches('>').to_string()
}
