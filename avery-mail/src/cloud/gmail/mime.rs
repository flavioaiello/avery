use super::charset;
use super::header;
use super::html;
use super::message;
use crate::backend::Attachment;
use crate::base64;
use crate::json::{Json, Object};

const MAX_DEPTH: usize = 12;
const MAX_PARTS: usize = 300;
pub(crate) const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024;
const MAX_ATTACHMENTS: usize = 100;
const MAX_MISSING: usize = 2;
const MAX_NAME_CHARS: usize = 200;
const MAX_PARAMETERS: usize = 8;
const MAX_CHARSET: usize = 64;
const MAX_ATTACHMENT_ID: usize = 2048;

pub(crate) struct Body {
    pub(crate) text: String,
    pub(crate) attachments: Vec<Attachment>,
    pub(crate) missing: Vec<String>,
}

struct Part<'a> {
    mime: String,
    charset: String,
    filename: &'a str,
    size: Option<i64>,
    data: Option<&'a str>,
    attachment_id: Option<&'a str>,
    children: Vec<Part<'a>>,
}

#[derive(Default)]
struct Found {
    text: String,
    plain: bool,
}

fn parameter(value: &str, wanted: &str) -> Option<String> {
    value
        .split(';')
        .skip(1)
        .take(MAX_PARAMETERS)
        .find_map(|piece| {
            let (name, found) = piece.split_once('=')?;
            name.trim().eq_ignore_ascii_case(wanted).then(|| {
                found
                    .trim()
                    .trim_matches('"')
                    .chars()
                    .take(MAX_CHARSET)
                    .collect()
            })
        })
}

fn mime_of(object: &Object, headers: Option<&Json>) -> String {
    let declared = object
        .get("mimeType")
        .and_then(Json::as_str)
        .or_else(|| message::header(headers, "Content-Type"))
        .unwrap_or_default();
    declared
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

fn tree<'a>(json: &'a Json, depth: usize, count: &mut usize) -> Option<Part<'a>> {
    if depth > MAX_DEPTH || *count >= MAX_PARTS {
        return None;
    }
    let object = json.as_object()?;
    *count += 1;
    let headers = object.get("headers");
    let body = object.get("body").and_then(Json::as_object);
    let mut children = Vec::new();
    for child in object
        .get("parts")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .take(MAX_PARTS)
    {
        if let Some(part) = tree(child, depth + 1, count) {
            children.push(part);
        }
    }
    Some(Part {
        mime: mime_of(object, headers),
        charset: message::header(headers, "Content-Type")
            .and_then(|value| parameter(value, "charset"))
            .unwrap_or_default(),
        filename: object
            .get("filename")
            .and_then(Json::as_str)
            .unwrap_or_default(),
        size: body
            .and_then(|body| message::whole(body.get("size")))
            .filter(|size| *size >= 0),
        data: body
            .and_then(|body| body.get("data"))
            .and_then(Json::as_str),
        attachment_id: body
            .and_then(|body| body.get("attachmentId"))
            .and_then(Json::as_str)
            .filter(|id| !id.is_empty() && id.len() <= MAX_ATTACHMENT_ID),
        children,
    })
}

struct Reader<'a> {
    budget: usize,
    fetched: &'a [(String, Vec<u8>)],
    missing: Vec<String>,
    attachments: Vec<Attachment>,
}

pub(crate) fn decode(data: &str, allowed: usize) -> Option<Vec<u8>> {
    if allowed == 0 {
        return None;
    }
    let limit = allowed.div_ceil(3).saturating_mul(4);
    let mut bytes = if data.len() <= limit {
        base64::decode(data)?
    } else {
        base64::decode(&data.chars().take(limit).collect::<String>())?
    };
    bytes.truncate(allowed);
    Some(bytes)
}

impl Reader<'_> {
    fn inline(&mut self, data: &str) -> Option<Vec<u8>> {
        let bytes = decode(data, self.budget)?;
        self.budget -= bytes.len();
        Some(bytes)
    }

    fn bytes(&mut self, part: &Part<'_>) -> Option<Vec<u8>> {
        if let Some(data) = part.data {
            return self.inline(data);
        }
        let id = part.attachment_id?;
        if let Some((_, known)) = self.fetched.iter().find(|(wanted, _)| wanted == id) {
            let mut bytes = known.clone();
            bytes.truncate(self.budget);
            self.budget -= bytes.len();
            return Some(bytes);
        }
        let plausible = part.size.is_some_and(|size| {
            size > 0 && usize::try_from(size).is_ok_and(|n| n <= MAX_TEXT_BYTES)
        });
        if plausible && self.missing.len() < MAX_MISSING && !self.missing.iter().any(|m| m == id) {
            self.missing.push(id.to_string());
        }
        None
    }

    fn leaf(&mut self, part: &Part<'_>, markup: bool) -> Found {
        let Some(bytes) = self.bytes(part) else {
            return Found::default();
        };
        let text = charset::decode(&part.charset, &bytes);
        Found {
            text: if markup { html::to_text(&text) } else { text },
            plain: !markup,
        }
    }

    fn attach(&mut self, part: &Part<'_>) {
        if self.attachments.len() >= MAX_ATTACHMENTS {
            return;
        }
        let name = header::text(part.filename, MAX_NAME_CHARS);
        self.attachments.push(Attachment {
            name: if name.is_empty() {
                "unnamed".to_string()
            } else {
                name
            },
            size: part.size,
        });
    }

    fn alternatives(&mut self, part: &Part<'_>, wanted: bool) -> Found {
        let mut best: Option<Found> = None;
        for child in &part.children {
            let settled = best.as_ref().is_some_and(|found| found.plain);
            let found = self.walk(child, wanted && !settled);
            if found.text.trim().is_empty() {
                continue;
            }
            let better = match &best {
                None => true,
                Some(current) => !current.plain && found.plain,
            };
            if better {
                best = Some(found);
            }
        }
        best.unwrap_or_default()
    }

    fn sequence(&mut self, part: &Part<'_>, wanted: bool) -> Found {
        let mut texts: Vec<String> = Vec::new();
        let mut plain = true;
        for child in &part.children {
            let found = self.walk(child, wanted);
            if !found.text.trim().is_empty() {
                plain &= found.plain;
                texts.push(found.text);
            }
        }
        Found {
            text: texts.join("\n\n"),
            plain,
        }
    }

    fn walk(&mut self, part: &Part<'_>, wanted: bool) -> Found {
        if !part.filename.is_empty() {
            self.attach(part);
            return Found::default();
        }
        match part.mime.as_str() {
            "text/plain" if wanted => self.leaf(part, false),
            "text/html" if wanted => self.leaf(part, true),
            "text/plain" | "text/html" => Found::default(),
            "multipart/alternative" => self.alternatives(part, wanted),
            _ if !part.children.is_empty() => self.sequence(part, wanted),
            _ => Found::default(),
        }
    }
}

pub(crate) fn extract(payload: Option<&Json>, fetched: &[(String, Vec<u8>)]) -> Body {
    let mut count = 0;
    let Some(root) = payload.and_then(|json| tree(json, 0, &mut count)) else {
        return Body {
            text: String::new(),
            attachments: Vec::new(),
            missing: Vec::new(),
        };
    };
    let mut reader = Reader {
        budget: MAX_TEXT_BYTES,
        fetched,
        missing: Vec::new(),
        attachments: Vec::new(),
    };
    let found = reader.walk(&root, true);
    Body {
        text: found.text,
        attachments: reader.attachments,
        missing: reader.missing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{j, TestResult};

    fn data(text: &str) -> String {
        base64::encode_url(text.as_bytes())
    }

    fn leaf(mime: &str, text: &str) -> String {
        format!(
            r#"{{"mimeType": "{mime}", "filename": "", "headers": [{{"name": "Content-Type", "value": "{mime}; charset=\"UTF-8\""}}], "body": {{"size": {}, "data": "{}"}}}}"#,
            text.len(),
            data(text)
        )
    }

    fn container(mime: &str, children: &[String]) -> String {
        format!(
            r#"{{"mimeType": "{mime}", "filename": "", "headers": [], "body": {{"size": 0}}, "parts": [{}]}}"#,
            children.join(", ")
        )
    }

    fn file(name: &str, mime: &str, size: i64) -> String {
        format!(
            r#"{{"mimeType": "{mime}", "filename": "{name}", "body": {{"attachmentId": "ANGj-{name}", "size": {size}}}}}"#
        )
    }

    fn read(payload: &str) -> Result<Body, Box<dyn std::error::Error>> {
        Ok(extract(Some(&j(payload)?), &[]))
    }

    fn names(body: &Body) -> Vec<(String, Option<i64>)> {
        body.attachments
            .iter()
            .map(|a| (a.name.clone(), a.size))
            .collect()
    }

    #[test]
    fn a_single_plain_text_part_is_decoded_with_or_without_padding() -> TestResult {
        for text in [
            "Hallo Welt",
            "Hallo Welt!",
            "Hallo Welt!!",
            "Gr\u{fc}\u{df}e \u{20ac}",
        ] {
            let body = read(&leaf("text/plain", text))?;
            assert_eq!(body.text, text);
            assert!(body.attachments.is_empty());
            assert!(body.missing.is_empty());
        }
        let padded = j(r#"{"mimeType": "text/plain", "body": {"data": "SGFsbG8gV2VsdA=="}}"#)?;
        assert_eq!(extract(Some(&padded), &[]).text, "Hallo Welt");
        let url_safe = j(r#"{"mimeType": "text/plain", "body": {"data": "Pz8-Pz8_"}}"#)?;
        assert_eq!(extract(Some(&url_safe), &[]).text, "??>???");
        Ok(())
    }

    #[test]
    fn alternatives_prefer_plain_text_in_any_order() -> TestResult {
        for children in [
            [
                leaf("text/plain", "plain"),
                leaf("text/html", "<p>html</p>"),
            ],
            [
                leaf("text/html", "<p>html</p>"),
                leaf("text/plain", "plain"),
            ],
        ] {
            let body = read(&container("multipart/alternative", &children))?;
            assert_eq!(body.text, "plain");
        }
        let blank_plain = [
            leaf("text/plain", " \r\n "),
            leaf("text/html", "<p>html</p>"),
        ];
        assert_eq!(
            read(&container("multipart/alternative", &blank_plain))?.text,
            "html"
        );
        let html_only = [leaf("text/html", "<p>Hallo<br>Welt</p>")];
        assert_eq!(
            read(&container("multipart/alternative", &html_only))?.text,
            "Hallo\nWelt"
        );
        Ok(())
    }

    #[test]
    fn html_only_mail_is_converted_to_text() -> TestResult {
        let html = "<html><head><style>p{}</style></head><body><p>Hi &amp; bye</p><script>x()</script></body></html>";
        assert_eq!(read(&leaf("text/html", html))?.text, "Hi & bye");
        Ok(())
    }

    #[test]
    fn nested_mixed_mail_lists_its_attachments_and_keeps_the_text() -> TestResult {
        let payload = container(
            "multipart/mixed",
            &[
                container(
                    "multipart/alternative",
                    &[
                        leaf("text/plain", "Siehe Anhang."),
                        container(
                            "multipart/related",
                            &[
                                leaf("text/html", "<p>Siehe <b>Anhang</b>.</p>"),
                                file("logo.png", "image/png", 2048),
                            ],
                        ),
                    ],
                ),
                file("Rechnung.pdf", "application/pdf", 123_456),
                file("notes.txt", "text/plain", 12),
                leaf("text/plain", "-- \nFooter"),
            ],
        );
        let body = read(&payload)?;
        assert_eq!(body.text, "Siehe Anhang.\n\n-- \nFooter");
        assert_eq!(
            names(&body),
            [
                ("logo.png".to_string(), Some(2048)),
                ("Rechnung.pdf".to_string(), Some(123_456)),
                ("notes.txt".to_string(), Some(12)),
            ]
        );
        Ok(())
    }

    #[test]
    fn a_text_part_with_a_filename_is_an_attachment_not_the_body() -> TestResult {
        let payload = container(
            "multipart/mixed",
            &[
                leaf("text/plain", "Body"),
                r#"{"mimeType": "text/plain", "filename": "data.txt", "body": {"size": 5, "data": "SGVsbG8"}}"#.to_string(),
            ],
        );
        let body = read(&payload)?;
        assert_eq!(body.text, "Body");
        assert_eq!(names(&body), [("data.txt".to_string(), Some(5))]);
        Ok(())
    }

    #[test]
    fn attachment_names_are_decoded_and_cleaned() -> TestResult {
        let payload = container(
            "multipart/mixed",
            &[
                file(
                    "=?UTF-8?B?UmVjaG51bmdfTcO8bGxlci5wZGY=?=",
                    "application/pdf",
                    1,
                ),
                file("a\\u202eb\\r\\nc.pdf", "application/pdf", 2),
                file("   ", "application/pdf", 3),
                file("\\u200b", "application/pdf", 4),
            ],
        );
        let body = read(&payload)?;
        assert_eq!(
            names(&body),
            [
                ("Rechnung_M\u{fc}ller.pdf".to_string(), Some(1)),
                ("ab c.pdf".to_string(), Some(2)),
                ("unnamed".to_string(), Some(3)),
                ("unnamed".to_string(), Some(4)),
            ]
        );
        Ok(())
    }

    #[test]
    fn declared_charsets_are_honoured() -> TestResult {
        let latin1 = format!(
            r#"{{"mimeType": "text/plain", "headers": [{{"name": "content-type", "value": "text/plain; format=flowed; charset=ISO-8859-1"}}], "body": {{"data": "{}"}}}}"#,
            base64::encode_url(&[0x4B, 0xE4, 0x73, 0x65])
        );
        assert_eq!(read(&latin1)?.text, "K\u{e4}se");
        let cp1252 = format!(
            r#"{{"mimeType": "text/plain", "headers": [{{"name": "Content-Type", "value": "text/plain; charset=\"windows-1252\""}}], "body": {{"data": "{}"}}}}"#,
            base64::encode_url(&[0x80, 0x20, 0x93, 0x78, 0x94])
        );
        assert_eq!(read(&cp1252)?.text, "\u{20ac} \u{201c}x\u{201d}");
        let unlabelled = format!(
            r#"{{"mimeType": "text/plain", "body": {{"data": "{}"}}}}"#,
            base64::encode_url(&[0x47, 0x72, 0xFC, 0xDF, 0x65])
        );
        assert_eq!(read(&unlabelled)?.text, "Gr\u{fc}\u{df}e");
        let broken_utf8 = format!(
            r#"{{"mimeType": "text/plain", "headers": [{{"name": "Content-Type", "value": "text/plain; charset=utf-8"}}], "body": {{"data": "{}"}}}}"#,
            base64::encode_url(&[0x61, 0xFF, 0x62])
        );
        assert_eq!(read(&broken_utf8)?.text, "a\u{fffd}b");
        Ok(())
    }

    #[test]
    fn the_mime_type_may_come_from_the_content_type_header() -> TestResult {
        let payload = format!(
            r#"{{"headers": [{{"name": "Content-Type", "value": "TEXT/PLAIN; charset=utf-8"}}], "body": {{"data": "{}"}}}}"#,
            data("via header")
        );
        assert_eq!(read(&payload)?.text, "via header");
        Ok(())
    }

    #[test]
    fn unknown_parts_and_empty_payloads_give_no_text() -> TestResult {
        let payload = container(
            "multipart/mixed",
            &[
                r#"{"mimeType": "image/png", "filename": "", "body": {"size": 10, "data": "AAAA"}}"#.to_string(),
                r#"{"mimeType": "text/calendar", "filename": "", "body": {"data": "QkVHSU46VkNBTEVOREFS"}}"#.to_string(),
            ],
        );
        let body = read(&payload)?;
        assert_eq!(body.text, "");
        assert!(body.attachments.is_empty());
        for empty in ["{}", "[]", "null", "\"x\"", "5", r#"{"parts": []}"#] {
            let body = extract(Some(&j(empty)?), &[]);
            assert_eq!(body.text, "", "{empty}");
        }
        assert_eq!(extract(None, &[]).text, "");
        Ok(())
    }

    #[test]
    fn malformed_fields_are_ignored() -> TestResult {
        let payload = r#"{"mimeType": "multipart/mixed", "filename": 5, "parts": [
            null, 7, "x", [],
            {"mimeType": 5, "body": {"data": "SGk"}},
            {"mimeType": "text/plain", "body": "nope"},
            {"mimeType": "text/plain", "body": {"data": 5}},
            {"mimeType": "text/plain", "body": {"data": "!!!not base64!!!"}},
            {"mimeType": "text/plain", "headers": "x", "body": {"data": "T0s"}},
            {"mimeType": "multipart/alternative", "parts": "nope"}
        ]}"#;
        let body = read(payload)?;
        assert_eq!(body.text, "OK");
        Ok(())
    }

    #[test]
    fn at_most_the_part_limit_is_examined() -> TestResult {
        let children: Vec<String> = (0..MAX_PARTS + 100)
            .map(|_| leaf("text/plain", "x"))
            .collect();
        let body = read(&container("multipart/mixed", &children))?;
        assert_eq!(body.text.split("\n\n").count(), MAX_PARTS - 1);
        let files: Vec<String> = (0..MAX_PARTS + 100)
            .map(|n| file(&format!("f{n}"), "application/pdf", 1))
            .collect();
        let body = read(&container("multipart/mixed", &files))?;
        assert_eq!(body.attachments.len(), MAX_ATTACHMENTS);
        Ok(())
    }

    #[test]
    fn nesting_beyond_the_depth_limit_is_not_followed() -> TestResult {
        let nested = |levels: usize| {
            let mut payload = leaf("text/plain", "deep");
            for _ in 0..levels {
                payload = container("multipart/mixed", &[payload]);
            }
            payload
        };
        assert_eq!(read(&nested(MAX_DEPTH))?.text, "deep");
        assert_eq!(read(&nested(MAX_DEPTH + 1))?.text, "");
        assert_eq!(read(&nested(50))?.text, "");
        Ok(())
    }

    #[test]
    fn decoded_text_is_cut_at_the_size_limit_and_shared_between_parts() -> TestResult {
        let big = "a".repeat(MAX_TEXT_BYTES + 5000);
        assert_eq!(read(&leaf("text/plain", &big))?.text.len(), MAX_TEXT_BYTES);
        let first = "b".repeat(MAX_TEXT_BYTES / 2 + 100);
        let second = "c".repeat(MAX_TEXT_BYTES / 2 + 100);
        let body = read(&container(
            "multipart/mixed",
            &[leaf("text/plain", &first), leaf("text/plain", &second)],
        ))?;
        assert_eq!(body.text.len(), MAX_TEXT_BYTES + 2);
        assert_eq!(body.text.matches('b').count(), first.len());
        assert_eq!(body.text.matches('c').count(), MAX_TEXT_BYTES - first.len());
        let third = read(&container(
            "multipart/mixed",
            &[leaf("text/plain", &big), leaf("text/plain", "late")],
        ))?;
        assert!(!third.text.contains("late"));
        Ok(())
    }

    #[test]
    fn large_text_parts_without_inline_data_are_reported_and_can_be_supplied() -> TestResult {
        let payload = container(
            "multipart/alternative",
            &[r#"{"mimeType": "text/html", "filename": "", "body": {"attachmentId": "ANGj-big", "size": 90000}}"#.to_string()],
        );
        let first = read(&payload)?;
        assert_eq!(first.text, "");
        assert_eq!(first.missing, ["ANGj-big"]);
        let fetched = vec![("ANGj-big".to_string(), b"<p>Lang</p>".to_vec())];
        let second = extract(Some(&j(&payload)?), &fetched);
        assert_eq!(second.text, "Lang");
        assert!(second.missing.is_empty());
        let other = vec![("other".to_string(), b"x".to_vec())];
        assert_eq!(extract(Some(&j(&payload)?), &other).missing, ["ANGj-big"]);
        Ok(())
    }

    #[test]
    fn only_plausible_text_parts_are_reported_as_missing() -> TestResult {
        let part = |id: &str, size: &str| {
            format!(
                r#"{{"mimeType": "text/plain", "filename": "", "body": {{"attachmentId": "{id}", "size": {size}}}}}"#
            )
        };
        let many: Vec<String> = (0..6).map(|n| part(&format!("id{n}"), "100")).collect();
        let body = read(&container("multipart/mixed", &many))?;
        assert_eq!(body.missing, ["id0", "id1"]);
        let odd = [
            part("same", "100"),
            part("same", "100"),
            part("zero", "0"),
            part("huge", &(MAX_TEXT_BYTES + 1).to_string()),
            part("negative", "-5"),
            r#"{"mimeType": "text/plain", "body": {"size": 10}}"#.to_string(),
            r#"{"mimeType": "text/plain", "body": {"attachmentId": "", "size": 10}}"#.to_string(),
        ];
        let body = read(&container("multipart/mixed", &odd))?;
        assert_eq!(body.missing, ["same"]);
        let skipped = [leaf("text/plain", "plain"), part("later", "100")];
        let body = read(&container("multipart/alternative", &skipped))?;
        assert_eq!(body.text, "plain");
        assert!(body.missing.is_empty());
        Ok(())
    }

    #[test]
    fn attachments_of_a_skipped_alternative_are_still_listed() -> TestResult {
        let payload = container(
            "multipart/alternative",
            &[
                leaf("text/plain", "plain"),
                container(
                    "multipart/related",
                    &[
                        leaf("text/html", "<p>html</p>"),
                        file("cid.png", "image/png", 9),
                    ],
                ),
            ],
        );
        let body = read(&payload)?;
        assert_eq!(body.text, "plain");
        assert_eq!(names(&body), [("cid.png".to_string(), Some(9))]);
        Ok(())
    }

    #[test]
    fn forwarded_messages_are_walked_like_containers() -> TestResult {
        let payload = container(
            "multipart/mixed",
            &[
                leaf("text/plain", "See below"),
                container("message/rfc822", &[leaf("text/plain", "Inner text")]),
            ],
        );
        assert_eq!(read(&payload)?.text, "See below\n\nInner text");
        Ok(())
    }

    #[test]
    fn sizes_must_be_non_negative_whole_numbers() -> TestResult {
        let payload = container(
            "multipart/mixed",
            &[
                r#"{"mimeType": "application/pdf", "filename": "a", "body": {"size": -4}}"#
                    .to_string(),
                r#"{"mimeType": "application/pdf", "filename": "b", "body": {"size": "12"}}"#
                    .to_string(),
                r#"{"mimeType": "application/pdf", "filename": "c", "body": {"size": 1.5}}"#
                    .to_string(),
                r#"{"mimeType": "application/pdf", "filename": "d"}"#.to_string(),
            ],
        );
        let body = read(&payload)?;
        assert_eq!(
            names(&body),
            [
                ("a".to_string(), None),
                ("b".to_string(), Some(12)),
                ("c".to_string(), None),
                ("d".to_string(), None),
            ]
        );
        Ok(())
    }

    #[test]
    fn content_type_parameters_are_parsed_leniently() {
        assert_eq!(
            parameter("text/plain; charset=UTF-8", "charset").as_deref(),
            Some("UTF-8")
        );
        assert_eq!(
            parameter(
                "text/plain; format=flowed; CHARSET=\"iso-8859-1\"",
                "charset"
            )
            .as_deref(),
            Some("iso-8859-1")
        );
        assert_eq!(parameter("text/plain", "charset"), None);
        assert_eq!(parameter("text/plain; charset", "charset"), None);
        assert_eq!(
            parameter(
                "text/plain; x=1; y=2; z=3; a=4; b=5; c=6; d=7; e=8; charset=x",
                "charset"
            ),
            None
        );
        assert_eq!(parameter("charset=utf-8", "charset"), None);
        assert_eq!(
            parameter(
                &format!("text/plain; charset={}", "x".repeat(500)),
                "charset"
            )
            .map(|c| c.len()),
            Some(MAX_CHARSET)
        );
    }
}
