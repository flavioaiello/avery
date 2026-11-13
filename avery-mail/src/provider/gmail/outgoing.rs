use super::header;
use crate::base64;

const LINE: usize = 76;
const WORD_BYTES: usize = 39;

pub(crate) struct Outgoing<'a> {
    pub(crate) to: &'a [String],
    pub(crate) cc: &'a [String],
    pub(crate) subject: &'a str,
    pub(crate) in_reply_to: Option<&'a str>,
    pub(crate) references: &'a [String],
    pub(crate) body: &'a str,
}

fn address_ok(address: &str) -> bool {
    !address.is_empty()
        && address
            .chars()
            .all(|c| c.is_ascii_graphic() && !"<>(),;:\"\\[]".contains(c))
}

fn fold(name: &str, tokens: &[String]) -> String {
    let mut out = String::new();
    let mut line = format!("{name}:");
    for (index, token) in tokens.iter().enumerate() {
        if index > 0 && line.len() + 1 + token.len() > LINE {
            out.push_str(&line);
            out.push_str("\r\n");
            line.clear();
        }
        line.push(' ');
        line.push_str(token);
    }
    out.push_str(&line);
    out.push_str("\r\n");
    out
}

fn address_list(name: &str, addresses: &[String]) -> String {
    let last = addresses.len().saturating_sub(1);
    let tokens: Vec<String> = addresses
        .iter()
        .enumerate()
        .map(|(index, address)| {
            if index < last {
                format!("{address},")
            } else {
                address.clone()
            }
        })
        .collect();
    fold(name, &tokens)
}

fn encoded_word(chunk: &str) -> String {
    format!("=?UTF-8?B?{}?=", base64::encode(chunk.as_bytes()))
}

fn encoded_words(subject: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut chunk = String::new();
    for c in subject.chars() {
        if chunk.len() + c.len_utf8() > WORD_BYTES {
            words.push(encoded_word(&chunk));
            chunk.clear();
        }
        chunk.push(c);
    }
    if !chunk.is_empty() {
        words.push(encoded_word(&chunk));
    }
    words
}

fn subject_header(subject: &str) -> String {
    let literal = !subject.contains("=?") && subject.chars().all(|c| (' '..='~').contains(&c));
    let tokens: Vec<String> = if literal {
        subject.split_whitespace().map(str::to_string).collect()
    } else {
        encoded_words(subject)
    };
    fold("Subject", &tokens)
}

fn body_lines(body: &str) -> String {
    let canonical = body
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', "\r\n");
    let encoded = base64::encode(canonical.as_bytes());
    let mut out = String::with_capacity(encoded.len() + encoded.len() / LINE * 2 + 2);
    for chunk in encoded.as_bytes().chunks(LINE) {
        if let Ok(line) = std::str::from_utf8(chunk) {
            out.push_str(line);
            out.push_str("\r\n");
        }
    }
    out
}

impl Outgoing<'_> {
    fn safe(&self) -> bool {
        self.to
            .iter()
            .chain(self.cc)
            .all(|address| address_ok(address))
            && !self.to.is_empty()
            && !self.subject.chars().any(char::is_control)
            && self.in_reply_to.is_none_or(header::message_id_ok)
            && self
                .references
                .iter()
                .all(|reference| header::message_id_ok(reference))
    }

    pub(crate) fn raw(&self) -> Option<String> {
        if !self.safe() {
            return None;
        }
        let mut message = address_list("To", self.to);
        if !self.cc.is_empty() {
            message.push_str(&address_list("Cc", self.cc));
        }
        if !self.subject.trim().is_empty() {
            message.push_str(&subject_header(self.subject));
        }
        if let Some(parent) = self.in_reply_to {
            message.push_str(&fold("In-Reply-To", &[parent.to_string()]));
        }
        if !self.references.is_empty() {
            message.push_str(&fold("References", self.references));
        }
        message.push_str("MIME-Version: 1.0\r\n");
        message.push_str("Content-Type: text/plain; charset=UTF-8\r\n");
        message.push_str("Content-Transfer-Encoding: base64\r\n");
        message.push_str("\r\n");
        message.push_str(&body_lines(self.body));
        Some(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestResult;

    fn list(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    fn mail<'a>(to: &'a [String], subject: &'a str, body: &'a str) -> Outgoing<'a> {
        Outgoing {
            to,
            cc: &[],
            subject,
            in_reply_to: None,
            references: &[],
            body,
        }
    }

    fn split(raw: &str) -> Option<(Vec<String>, String)> {
        let (head, body) = raw.split_once("\r\n\r\n")?;
        let mut headers: Vec<String> = Vec::new();
        for line in head.split("\r\n") {
            if line.starts_with(' ') {
                let last = headers.last_mut()?;
                last.push_str(line);
            } else {
                headers.push(line.to_string());
            }
        }
        Some((headers, body.to_string()))
    }

    fn refused(outgoing: Outgoing) -> bool {
        outgoing.raw().is_none()
    }

    fn decoded_body(body: &str) -> Option<String> {
        let bytes = base64::decode(body)?;
        String::from_utf8(bytes).ok()
    }

    fn subject_value(raw: &str) -> Option<String> {
        let (headers, _) = split(raw)?;
        headers
            .iter()
            .find_map(|header| header.strip_prefix("Subject: "))
            .map(str::to_string)
    }

    fn decoded_words(value: &str) -> Option<String> {
        let mut out = String::new();
        for token in value.split_whitespace() {
            let inner = token.strip_prefix("=?UTF-8?B?")?.strip_suffix("?=")?;
            out.push_str(&String::from_utf8(base64::decode(inner)?).ok()?);
        }
        Some(out)
    }

    #[test]
    fn a_plain_message_has_exactly_the_documented_headers() -> TestResult {
        let to = list(&["anna@example.org"]);
        let raw = mail(&to, "Hallo", "Kurzer Text.").raw().ok_or("raw")?;
        assert_eq!(
            raw,
            concat!(
                "To: anna@example.org\r\n",
                "Subject: Hallo\r\n",
                "MIME-Version: 1.0\r\n",
                "Content-Type: text/plain; charset=UTF-8\r\n",
                "Content-Transfer-Encoding: base64\r\n",
                "\r\n",
                "S3VyemVyIFRleHQu\r\n"
            )
        );
        assert!(!raw.contains("From:"));
        Ok(())
    }

    #[test]
    fn cc_threading_headers_and_an_empty_subject_follow_the_inputs() -> TestResult {
        let to = list(&["a@example.org", "b@example.org"]);
        let cc = list(&["c@example.org"]);
        let references = list(&["<one@x.example>", "<two@x.example>"]);
        let raw = Outgoing {
            to: &to,
            cc: &cc,
            subject: "Re: Plan",
            in_reply_to: Some("<two@x.example>"),
            references: &references,
            body: "Ja.",
        }
        .raw()
        .ok_or("raw")?;
        let (headers, body) = split(&raw).ok_or("split")?;
        assert_eq!(
            headers,
            [
                "To: a@example.org, b@example.org",
                "Cc: c@example.org",
                "Subject: Re: Plan",
                "In-Reply-To: <two@x.example>",
                "References: <one@x.example> <two@x.example>",
                "MIME-Version: 1.0",
                "Content-Type: text/plain; charset=UTF-8",
                "Content-Transfer-Encoding: base64",
            ]
        );
        assert_eq!(decoded_body(&body).as_deref(), Some("Ja."));
        let bare = mail(&to, "", "x").raw().ok_or("raw")?;
        assert!(!bare.contains("Subject"));
        assert!(!bare.contains("Cc:"));
        assert!(!bare.contains("In-Reply-To"));
        assert!(!bare.contains("References"));
        let blank = mail(&to, "   ", "x").raw().ok_or("raw")?;
        assert!(!blank.contains("Subject"));
        Ok(())
    }

    #[test]
    fn long_recipient_lists_are_folded_below_the_line_limit() -> TestResult {
        let to: Vec<String> = (0..12)
            .map(|n| format!("recipient{n}@example.org"))
            .collect();
        let raw = mail(&to, "x", "y").raw().ok_or("raw")?;
        for line in raw.split("\r\n") {
            assert!(line.len() <= LINE, "{line}");
        }
        let (headers, _) = split(&raw).ok_or("split")?;
        let first = headers.first().ok_or("to")?;
        assert_eq!(first, &format!("To: {}", to.join(", ")));
        assert!(raw.contains(",\r\n recipient"));
        Ok(())
    }

    #[test]
    fn an_address_longer_than_a_line_is_not_split() -> TestResult {
        let long = format!("{}@example.org", "a".repeat(64));
        let to = vec!["first@example.org".to_string(), long.clone()];
        let raw = mail(&to, "x", "y").raw().ok_or("raw")?;
        assert!(raw.contains(&format!(",\r\n {long}\r\n")));
        let only = vec![long.clone()];
        assert!(mail(&only, "x", "y")
            .raw()
            .ok_or("raw")?
            .starts_with(&format!("To: {long}\r\n")));
        Ok(())
    }

    #[test]
    fn a_long_ascii_subject_is_folded_at_spaces_and_unfolds_to_the_original() -> TestResult {
        let subject = (0..30)
            .map(|n| format!("word{n}"))
            .collect::<Vec<_>>()
            .join(" ");
        let to = list(&["a@example.org"]);
        let raw = mail(&to, &subject, "y").raw().ok_or("raw")?;
        for line in raw.split("\r\n") {
            assert!(line.len() <= LINE, "{line}");
        }
        let (headers, _) = split(&raw).ok_or("split")?;
        let found = headers
            .iter()
            .find(|h| h.starts_with("Subject:"))
            .ok_or("subject")?;
        assert_eq!(found, &format!("Subject: {subject}"));
        assert!(raw.matches("\r\n word").count() >= 2);
        Ok(())
    }

    #[test]
    fn a_non_ascii_subject_is_sent_as_utf8_base64_encoded_words_below_the_line_limit() -> TestResult
    {
        let to = list(&["a@example.org"]);
        for subject in [
            "Gr\u{fc}\u{df}e",
            "Re: Gr\u{fc}\u{df}e aus Z\u{fc}rich mit einem ziemlich langen Betreff, der gefaltet werden muss",
            "\u{1f600}\u{1f600}\u{1f600}\u{1f600}\u{1f600}\u{1f600}\u{1f600}\u{1f600}\u{1f600}\u{1f600}\u{1f600}",
            "\u{4e2d}\u{6587}\u{4e2d}\u{6587}\u{4e2d}\u{6587}\u{4e2d}\u{6587}\u{4e2d}\u{6587}\u{4e2d}\u{6587}\u{4e2d}\u{6587}",
            &"\u{e4}".repeat(200),
        ] {
            let raw = mail(&to, subject, "y").raw().ok_or("raw")?;
            for line in raw.split("\r\n") {
                assert!(line.len() <= LINE, "{line}");
            }
            let (headers, _) = split(&raw).ok_or("split")?;
            let found = headers.iter().find(|h| h.starts_with("Subject:")).ok_or("subject")?;
            let value = found.trim_start_matches("Subject:");
            assert!(value.is_ascii());
            assert_eq!(header::text(value, 1000), subject, "{subject}");
            for word in value.split_whitespace() {
                assert!(word.starts_with("=?UTF-8?B?") && word.ends_with("?="), "{word}");
                assert!(word.len() <= 75, "{word}");
            }
        }
        Ok(())
    }

    #[test]
    fn a_subject_with_the_encoded_word_marker_is_sent_as_encoded_text_of_itself() -> TestResult {
        let to = list(&["a@example.org"]);
        let word = format!(
            "=?UTF-8?B?{}?=",
            base64::encode("Plan \u{202e}fdp.exe\u{200b}".as_bytes())
        );
        let subjects = [
            word.clone(),
            format!("Re: {word}"),
            format!("Re: {word} {word} {word} {word}"),
            "=?utf-8?q?Gr=C3=BC=C3=9Fe?=".to_string(),
            "=?".to_string(),
            "x=?".to_string(),
            "a =? b".to_string(),
            "Gr\u{fc}\u{df}e =?".to_string(),
        ];
        for subject in subjects {
            let raw = mail(&to, &subject, "y").raw().ok_or("raw")?;
            for line in raw.split("\r\n") {
                assert!(line.len() <= LINE, "{line}");
            }
            let value = subject_value(&raw).ok_or("subject")?;
            assert!(value.is_ascii(), "{subject}");
            assert!(!value.contains("=?UTF-8?B?UGxhbi"), "{subject}");
            assert_eq!(decoded_words(&value).as_deref(), Some(subject.as_str()));
            assert_eq!(header::text(&value, 1000), subject);
            for token in value.split_whitespace() {
                assert!(token.len() <= 75, "{token}");
            }
        }
        Ok(())
    }

    #[test]
    fn ascii_subjects_without_the_marker_stay_plain_text() -> TestResult {
        let to = list(&["a@example.org"]);
        for subject in ["Re: Plan", "?=", "a ?= b", "= ?", "Was? =)", "100% = 1"] {
            let raw = mail(&to, subject, "y").raw().ok_or("raw")?;
            assert_eq!(subject_value(&raw).as_deref(), Some(subject));
        }
        Ok(())
    }

    #[test]
    fn an_encoded_word_never_splits_a_character() {
        for subject in [
            "\u{1f600}".repeat(30),
            "\u{e4}".repeat(50),
            "a\u{20ac}".repeat(30),
        ] {
            for word in encoded_words(&subject) {
                let inner = word.trim_start_matches("=?UTF-8?B?").trim_end_matches("?=");
                let bytes = base64::decode(inner).unwrap_or_default();
                assert!(String::from_utf8(bytes.clone()).is_ok(), "{word}");
                assert!(bytes.len() <= WORD_BYTES);
            }
        }
        assert!(encoded_words("").is_empty());
    }

    #[test]
    fn the_body_is_utf8_base64_wrapped_at_76_columns_with_crlf_line_ends() -> TestResult {
        let to = list(&["a@example.org"]);
        let text = "Zeile eins\n\nGr\u{fc}\u{df}e aus Z\u{fc}rich \u{20ac}\n".repeat(40);
        let raw = mail(&to, "x", &text).raw().ok_or("raw")?;
        let (_, body) = split(&raw).ok_or("split")?;
        assert!(body.ends_with("\r\n"));
        let lines: Vec<&str> = body.trim_end_matches("\r\n").split("\r\n").collect();
        let (last, full) = lines.split_last().ok_or("lines")?;
        assert!(full.iter().all(|line| line.len() == LINE));
        assert!(last.len() <= LINE && !last.is_empty());
        let wanted = text.replace('\n', "\r\n");
        assert_eq!(decoded_body(&body).as_deref(), Some(wanted.as_str()));
        Ok(())
    }

    #[test]
    fn every_kind_of_line_end_in_the_body_becomes_crlf() -> TestResult {
        let to = list(&["a@example.org"]);
        let raw = mail(&to, "x", "a\nb\r\nc\rd\n\ne").raw().ok_or("raw")?;
        let (_, body) = split(&raw).ok_or("split")?;
        assert_eq!(
            decoded_body(&body).as_deref(),
            Some("a\r\nb\r\nc\r\nd\r\n\r\ne")
        );
        let bare = raw.replace("\r\n", "");
        assert!(!bare.contains('\n') && !bare.contains('\r'));
        Ok(())
    }

    #[test]
    fn an_empty_body_gives_an_empty_part() -> TestResult {
        let to = list(&["a@example.org"]);
        let raw = mail(&to, "x", "").raw().ok_or("raw")?;
        assert!(raw.ends_with("base64\r\n\r\n"));
        Ok(())
    }

    #[test]
    fn unsafe_values_are_refused_instead_of_written() {
        let good = list(&["a@example.org"]);
        for subject in [
            "a\r\nBcc: x@example.org",
            "a\nb",
            "a\rb",
            "a\tb",
            "a\u{0}b",
            "a\u{7f}b",
            "a\u{85}b",
        ] {
            assert!(refused(mail(&good, subject, "y")), "{subject:?}");
        }
        for address in [
            "a@example.org\r\nBcc: x@example.org",
            "a@example.org\n",
            "a b@example.org",
            "a@example.org>",
            "<a@example.org>",
            "a@example.org,b@example.org",
            "a@example.org;b@example.org",
            "\"a\"@example.org",
            "\u{e4}@example.org",
            "",
            "a@exa\u{0}mple.org",
        ] {
            let bad = list(&[address]);
            assert!(refused(mail(&bad, "x", "y")), "{address:?}");
            let cc_bad = Outgoing {
                to: &good,
                cc: &bad,
                subject: "x",
                in_reply_to: None,
                references: &[],
                body: "y",
            };
            assert!(refused(cc_bad), "{address:?}");
        }
        assert!(refused(mail(&[], "x", "y")));
        for parent in ["x@y.example", "<a b>", "<a\r\nBcc: x>", ""] {
            let outgoing = Outgoing {
                to: &good,
                cc: &[],
                subject: "x",
                in_reply_to: Some(parent),
                references: &[],
                body: "y",
            };
            assert!(refused(outgoing), "{parent:?}");
        }
        let bad_refs = list(&["<ok@x.example>", "<a\r\nBcc: x>"]);
        let outgoing = Outgoing {
            to: &good,
            cc: &[],
            subject: "x",
            in_reply_to: None,
            references: &bad_refs,
            body: "y",
        };
        assert!(refused(outgoing));
    }

    #[test]
    fn hostile_text_in_the_body_cannot_reach_the_headers() -> TestResult {
        let to = list(&["a@example.org"]);
        let raw = mail(&to, "x", "\r\nBcc: evil@example.org\r\n\r\nInjected")
            .raw()
            .ok_or("raw")?;
        assert!(!raw.contains("Bcc"));
        let (headers, body) = split(&raw).ok_or("split")?;
        assert_eq!(headers.len(), 5);
        assert_eq!(
            decoded_body(&body).as_deref(),
            Some("\r\nBcc: evil@example.org\r\n\r\nInjected")
        );
        Ok(())
    }
}
