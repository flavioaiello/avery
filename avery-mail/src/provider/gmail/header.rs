use super::charset;
use crate::backend::Person;
use crate::base64;
use crate::text::{one_line_str, take_chars, MAX_ADDRESS};

const MAX_HEADER_CHARS: usize = 8192;
const MAX_WORD: usize = 1024;
const MAX_CHARSET: usize = 40;
const MAX_PEOPLE: usize = 100;
const MAX_NAME_CHARS: usize = 200;
const MAX_IDS: usize = 100;
const MAX_ID_CHARS: usize = 255;

struct Word {
    charset: String,
    bytes: Vec<u8>,
    length: usize,
}

fn hex(byte: u8) -> Option<u8> {
    char::from(byte)
        .to_digit(16)
        .and_then(|digit| u8::try_from(digit).ok())
}

fn q_decode(data: &str) -> Option<Vec<u8>> {
    let bytes = data.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(byte) = bytes.get(index) {
        match byte {
            b'_' => {
                out.push(b' ');
                index += 1;
            }
            b'=' => {
                let high = hex(bytes.get(index + 1).copied()?)?;
                let low = hex(bytes.get(index + 2).copied()?)?;
                out.push(high * 16 + low);
                index += 3;
            }
            other => {
                out.push(*other);
                index += 1;
            }
        }
    }
    Some(out)
}

fn word(text: &str) -> Option<Word> {
    let body = text.strip_prefix("=?")?;
    let mut parts = body.splitn(3, '?');
    let charset = parts.next()?;
    let encoding = parts.next()?;
    let rest = parts.next()?;
    let usable = !charset.is_empty()
        && charset.len() <= MAX_CHARSET
        && charset.chars().all(|c| c.is_ascii_graphic());
    if !usable || !matches!(encoding, "B" | "b" | "Q" | "q") {
        return None;
    }
    let end = rest
        .as_bytes()
        .windows(2)
        .take(MAX_WORD + 2)
        .position(|pair| pair == b"?=")?;
    let data = rest.get(..end)?;
    if data.chars().any(char::is_whitespace) {
        return None;
    }
    let bytes = match encoding {
        "B" | "b" => base64::decode(data)?,
        _ => q_decode(data)?,
    };
    Some(Word {
        charset: charset.to_string(),
        bytes,
        length: 2 + charset.len() + 1 + encoding.len() + 1 + end + 2,
    })
}

struct Decoder {
    out: String,
    run: Option<(String, Vec<u8>)>,
    held: String,
}

impl Decoder {
    fn new(capacity: usize) -> Decoder {
        Decoder {
            out: String::with_capacity(capacity),
            run: None,
            held: String::new(),
        }
    }

    fn flush(&mut self) {
        if let Some((charset, bytes)) = self.run.take() {
            self.out.push_str(&charset::decode(&charset, &bytes));
        }
    }

    fn plain(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.run.is_some() && text.chars().all(char::is_whitespace) {
            self.held.push_str(text);
            return;
        }
        self.flush();
        self.out.push_str(&self.held);
        self.held.clear();
        self.out.push_str(text);
    }

    fn word(&mut self, found: Word) {
        self.held.clear();
        if let Some((charset, bytes)) = self.run.as_mut() {
            if *charset == found.charset {
                bytes.extend_from_slice(&found.bytes);
                return;
            }
        }
        self.flush();
        self.run = Some((found.charset, found.bytes));
    }

    fn finish(mut self) -> String {
        self.flush();
        self.out.push_str(&self.held);
        self.out
    }
}

fn decode_words(text: &str) -> String {
    let mut decoder = Decoder::new(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        let Some(at) = rest.find("=?") else {
            decoder.plain(rest);
            break;
        };
        decoder.plain(rest.get(..at).unwrap_or_default());
        let tail = rest.get(at..).unwrap_or_default();
        match word(tail) {
            Some(found) => {
                let length = found.length;
                decoder.word(found);
                rest = tail.get(length..).unwrap_or_default();
            }
            None => {
                decoder.plain("=?");
                rest = tail.get(2..).unwrap_or_default();
            }
        }
    }
    decoder.finish()
}

pub(crate) fn text(raw: &str, limit: usize) -> String {
    one_line_str(&decode_words(&take_chars(raw, MAX_HEADER_CHARS)), limit)
}

fn entries(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    let mut angle = false;
    let mut comment = 0usize;
    for c in text.chars() {
        if escaped {
            current.push(c);
            escaped = false;
            continue;
        }
        let open = !quoted && !angle && comment == 0;
        match c {
            '\\' if quoted || comment > 0 => {
                current.push(c);
                escaped = true;
            }
            '"' if comment == 0 && !angle => {
                quoted = !quoted;
                current.push(c);
            }
            '(' if !quoted && !angle => {
                comment += 1;
                current.push(c);
            }
            ')' if !quoted && comment > 0 => {
                comment -= 1;
                current.push(c);
            }
            '<' if !quoted && comment == 0 => {
                angle = true;
                current.push(c);
            }
            '>' if !quoted && comment == 0 => {
                angle = false;
                current.push(c);
            }
            ',' | ';' if open => out.push(std::mem::take(&mut current)),
            ':' if open => current.clear(),
            other => current.push(other),
        }
    }
    out.push(current);
    out
}

fn address_ok(candidate: &str) -> bool {
    let Some((local, domain)) = candidate.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && !domain.is_empty()
        && !domain.contains('@')
        && candidate.len() <= MAX_ADDRESS
        && candidate
            .chars()
            .all(|c| !c.is_control() && !c.is_whitespace() && !"<>(),;:\\".contains(c))
}

fn unquote(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut escaped = false;
    for c in name.trim().chars() {
        if escaped {
            out.push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c != '"' {
            out.push(c);
        }
    }
    out
}

fn angle_split(entry: &str) -> Option<(&str, &str)> {
    let mut quoted = false;
    let mut escaped = false;
    let mut comment = 0usize;
    let mut open = None;
    for (index, c) in entry.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if quoted || comment > 0 => escaped = true,
            '"' if comment == 0 => quoted = !quoted,
            '(' if !quoted => comment += 1,
            ')' if !quoted && comment > 0 => comment -= 1,
            '<' if !quoted && comment == 0 && open.is_none() => open = Some(index),
            '>' if !quoted && comment == 0 => {
                if let Some(start) = open {
                    return Some((entry.get(..start)?, entry.get(start + 1..index)?));
                }
            }
            _ => {}
        }
    }
    None
}

fn strip_comments(entry: &str) -> (String, Vec<String>) {
    let mut plain = String::with_capacity(entry.len());
    let mut comments: Vec<String> = Vec::new();
    let mut quoted = false;
    let mut escaped = false;
    let mut depth = 0usize;
    let mut current = String::new();
    for c in entry.chars() {
        if escaped {
            if depth > 0 {
                current.push(c);
            } else {
                plain.push(c);
            }
            escaped = false;
            continue;
        }
        match c {
            '\\' if quoted || depth > 0 => {
                escaped = true;
                if depth == 0 {
                    plain.push(c);
                }
            }
            '"' if depth == 0 => {
                quoted = !quoted;
                plain.push(c);
            }
            '(' if !quoted => {
                depth += 1;
                if depth > 1 {
                    current.push(c);
                }
            }
            ')' if !quoted && depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    comments.push(std::mem::take(&mut current));
                } else {
                    current.push(c);
                }
            }
            other if depth > 0 => current.push(other),
            other => plain.push(other),
        }
    }
    (plain, comments)
}

fn bare(entry: &str) -> Option<(String, String)> {
    let (plain, comments) = strip_comments(entry);
    let plain = plain.trim();
    let from_comment = comments.first().cloned().unwrap_or_default();
    if address_ok(plain) {
        return Some((from_comment, plain.to_string()));
    }
    let (name, last) = plain.rsplit_once(char::is_whitespace)?;
    let last = last.trim();
    address_ok(last).then(|| (name.trim().to_string(), last.to_string()))
}

fn person(entry: &str) -> Option<Person> {
    let (display, address) = match angle_split(entry) {
        Some((before, inside)) => {
            let (plain, comments) = strip_comments(before);
            let name = if plain.trim().is_empty() {
                comments.first().cloned().unwrap_or_default()
            } else {
                plain
            };
            (name, inside.trim().to_string())
        }
        None => bare(entry)?,
    };
    if !address_ok(&address) {
        return None;
    }
    let name = text(&unquote(&display), MAX_NAME_CHARS);
    let name = if name.eq_ignore_ascii_case(&address) {
        String::new()
    } else {
        name
    };
    Some(Person { name, address })
}

pub(crate) fn people(raw: &str) -> Vec<Person> {
    entries(&take_chars(raw, MAX_HEADER_CHARS))
        .iter()
        .filter_map(|entry| person(entry))
        .take(MAX_PEOPLE)
        .collect()
}

fn id_ok(inner: &str) -> bool {
    !inner.is_empty()
        && inner.len() <= MAX_ID_CHARS
        && inner.chars().all(|c| c.is_ascii_graphic() && c != '<')
}

pub(crate) fn message_id_ok(token: &str) -> bool {
    token
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix('>'))
        .is_some_and(id_ok)
}

pub(crate) fn message_ids(raw: &str) -> Vec<String> {
    let bounded = take_chars(raw, MAX_HEADER_CHARS);
    let mut out = Vec::new();
    let mut rest = bounded.as_str();
    while out.len() < MAX_IDS {
        let Some(start) = rest.find('<') else {
            break;
        };
        let after = rest.get(start + 1..).unwrap_or_default();
        let Some(end) = after.find('>') else {
            break;
        };
        let inner = after.get(..end).unwrap_or_default();
        if id_ok(inner) {
            out.push(format!("<{inner}>"));
        }
        rest = after.get(end + 1..).unwrap_or_default();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoded(raw: &str) -> String {
        text(raw, 500)
    }

    fn who(raw: &str) -> Vec<(String, String)> {
        people(raw)
            .into_iter()
            .map(|person| (person.name, person.address))
            .collect()
    }

    fn pair(name: &str, address: &str) -> (String, String) {
        (name.to_string(), address.to_string())
    }

    #[test]
    fn the_rfc_2047_examples_decode_as_specified() {
        for (raw, wanted) in [
            ("(=?ISO-8859-1?Q?a?=)", "(a)"),
            ("(=?ISO-8859-1?Q?a?= b)", "(a b)"),
            ("(=?ISO-8859-1?Q?a?= =?ISO-8859-1?Q?b?=)", "(ab)"),
            ("(=?ISO-8859-1?Q?a?=  =?ISO-8859-1?Q?b?=)", "(ab)"),
            ("(=?ISO-8859-1?Q?a?=\r\n =?ISO-8859-1?Q?b?=)", "(ab)"),
            ("(=?ISO-8859-1?Q?a_b?=)", "(a b)"),
            ("(=?ISO-8859-1?Q?a?= =?ISO-8859-2?Q?_b?=)", "(a b)"),
            (
                "=?US-ASCII?Q?Keith_Moore?= <moore@cs.utk.example>",
                "Keith Moore <moore@cs.utk.example>",
            ),
            (
                "=?ISO-8859-1?Q?Keld_J=F8rn_Simonsen?= <keld@dkuug.example>",
                "Keld Jørn Simonsen <keld@dkuug.example>",
            ),
            (
                "=?ISO-8859-1?Q?Andr=E9?= Pirard <PIRARD@vm1.ulg.example>",
                "André Pirard <PIRARD@vm1.ulg.example>",
            ),
            (
                "=?ISO-8859-1?B?SWYgeW91IGNhbiByZWFkIHRoaXMgeW8=?= =?ISO-8859-2?B?dSB1bmRlcnN0YW5kIHRoZSBleGFtcGxlLg==?=",
                "If you can read this you understand the example.",
            ),
        ] {
            assert_eq!(decoded(raw), wanted, "{raw}");
        }
    }

    #[test]
    fn b_and_q_words_in_utf8_and_latin1_are_decoded() {
        assert_eq!(decoded("=?UTF-8?B?R3LDvMOfZSDigqw=?="), "Grüße €");
        assert_eq!(
            decoded("=?utf-8?q?Gr=C3=BC=C3=9Fe_aus_Z=C3=BCrich?="),
            "Grüße aus Zürich"
        );
        assert_eq!(decoded("=?iso-8859-1?Q?K=E4se?="), "Käse");
        assert_eq!(decoded("=?ISO-8859-1?B?S+RzZQ==?="), "Käse");
        assert_eq!(decoded("=?UTF-8*de?Q?Hallo?="), "Hallo");
        assert_eq!(decoded("=?UTF-8?B?SGFsbG8?="), "Hallo");
    }

    #[test]
    fn a_character_split_over_two_words_is_reassembled() {
        assert_eq!(decoded("=?UTF-8?Q?Gr=C3?= =?UTF-8?Q?=BC=C3=9Fe?="), "Grüße");
        assert_eq!(decoded("=?UTF-8?B?w7w=?= =?UTF-8?B?w7w=?="), "üü");
        assert_eq!(decoded("=?UTF-8?Q?a?= =?ISO-8859-1?Q?=E4?="), "aä");
    }

    #[test]
    fn words_next_to_plain_text_keep_their_spaces() {
        assert_eq!(
            decoded("Re: =?UTF-8?Q?Gr=C3=BC=C3=9Fe?= aus Bern"),
            "Re: Grüße aus Bern"
        );
        assert_eq!(decoded("x=?UTF-8?Q?a?=y"), "xay");
        assert_eq!(decoded("  =?UTF-8?Q?a?=  "), "a");
        assert_eq!(decoded("=?UTF-8?Q?a?= text =?UTF-8?Q?b?="), "a text b");
    }

    #[test]
    fn malformed_encoded_words_stay_as_plain_text_and_never_panic() {
        for raw in [
            "=?",
            "=?=",
            "=??=",
            "=?UTF-8?",
            "=?UTF-8?Q",
            "=?UTF-8?Q?",
            "=?UTF-8?Q?abc",
            "=?UTF-8?X?abc?=",
            "=?UTF-8?B?!!!?=",
            "=?UTF-8?B?a?=",
            "=?UTF-8?Q?=G1?=",
            "=?UTF-8?Q?=",
            "=?UTF-8?Q?=1?=",
            "=?UTF-8?Q?a b?=",
            "=??Q?abc?=",
            "=?\u{e4}?Q?abc?=",
            "=?=?=?=?=?",
            "?=?=?=",
            "=?UTF-8?Q?abc?= =?",
            "=",
            "?",
        ] {
            let out = decoded(raw);
            assert!(out.chars().count() <= 500, "{raw}");
        }
        assert_eq!(decoded("=?UTF-8?B?!!!?="), "=?UTF-8?B?!!!?=");
        assert_eq!(decoded("=?UTF-8?Q?=G1?="), "=?UTF-8?Q?=G1?=");
        assert_eq!(decoded("=?UTF-8?X?abc?= x"), "=?UTF-8?X?abc?= x");
        assert_eq!(decoded("=?UTF-8?Q?a b?="), "=?UTF-8?Q?a b?=");
        assert_eq!(decoded("=?UTF-8?Q?abc"), "=?UTF-8?Q?abc");
        assert_eq!(decoded("a =?UTF-8?Q?"), "a =?UTF-8?Q?");
    }

    #[test]
    fn invalid_bytes_in_a_word_become_replacement_characters() {
        assert_eq!(decoded("=?UTF-8?Q?a=FFb?="), "a\u{FFFD}b");
        assert_eq!(decoded("=?UTF-8?B?/w==?="), "\u{FFFD}");
    }

    #[test]
    fn oversized_words_are_not_decoded_and_the_work_stays_bounded() {
        let long_word = format!("=?UTF-8?Q?{}?=", "a".repeat(MAX_WORD + 10));
        assert_eq!(
            decoded(&long_word),
            long_word.chars().take(500).collect::<String>() + " [...]"
        );
        let many = "=?a?B?".repeat(5000);
        assert!(decoded(&many).chars().count() <= 506);
        let all_valid = "=?UTF-8?Q?a?=".repeat(5000);
        assert!(decoded(&all_valid).chars().count() <= 506);
        let huge = "x".repeat(1_000_000);
        assert_eq!(decoded(&huge).chars().count(), 506);
        assert_eq!(text(&huge, 10), format!("{} [...]", "x".repeat(10)));
    }

    #[test]
    fn decoded_text_is_one_clean_line() {
        assert_eq!(decoded("a\r\nb\tc"), "a b c");
        assert_eq!(decoded("=?UTF-8?Q?a=0D=0Ab?="), "a b");
        assert_eq!(decoded("pass\u{202e}word\u{200b}!"), "password!");
        assert_eq!(decoded("=?UTF-8?B?4oCuYWJj?="), "abc");
        assert_eq!(decoded("   "), "");
        assert_eq!(decoded(""), "");
        assert_eq!(text("abcdefghij", 5), "abcde [...]");
    }

    #[test]
    fn plain_addresses_and_names_are_extracted() {
        assert_eq!(who("anna@example.org"), [pair("", "anna@example.org")]);
        assert_eq!(
            who("Anna Muster <anna@example.org>"),
            [pair("Anna Muster", "anna@example.org")]
        );
        assert_eq!(who("<anna@example.org>"), [pair("", "anna@example.org")]);
        assert_eq!(
            who("\"Muster, Anna\" <anna@example.org>, bob@example.org"),
            [
                pair("Muster, Anna", "anna@example.org"),
                pair("", "bob@example.org")
            ]
        );
        assert_eq!(
            who("A <a@example.org>,B <b@example.org> ,  C<c@example.org>"),
            [
                pair("A", "a@example.org"),
                pair("B", "b@example.org"),
                pair("C", "c@example.org")
            ]
        );
    }

    #[test]
    fn quoted_strings_keep_commas_angle_brackets_and_escapes() {
        assert_eq!(
            who(r#""Doe, John <boss>" <john@example.org>"#),
            [pair("Doe, John <boss>", "john@example.org")]
        );
        assert_eq!(
            who(r#""say \"hi\", you" <x@example.org>"#),
            [pair("say \"hi\", you", "x@example.org")]
        );
        assert_eq!(
            who(r#""back\\slash" <x@example.org>"#),
            [pair("back\\slash", "x@example.org")]
        );
        assert_eq!(
            who(r#""a@b.example" <a@b.example>"#),
            [pair("", "a@b.example")]
        );
    }

    #[test]
    fn encoded_names_are_decoded_even_inside_quotes() {
        assert_eq!(
            who("=?UTF-8?Q?M=C3=BCller=2C_Max?= <max@example.org>"),
            [pair("Müller, Max", "max@example.org")]
        );
        assert_eq!(
            who("\"=?UTF-8?B?TcO8bGxlcg==?=\" <max@example.org>"),
            [pair("Müller", "max@example.org")]
        );
    }

    #[test]
    fn comments_become_names_when_there_is_no_other_name() {
        assert_eq!(
            who("anna@example.org (Anna Muster)"),
            [pair("Anna Muster", "anna@example.org")]
        );
        assert_eq!(
            who("(Anna) <anna@example.org>"),
            [pair("Anna", "anna@example.org")]
        );
        assert_eq!(
            who("Anna (private, old) <anna@example.org>"),
            [pair("Anna", "anna@example.org")]
        );
        assert_eq!(
            who("anna@example.org (a (nested) comment, with comma), b@example.org"),
            [
                pair("a (nested) comment, with comma", "anna@example.org"),
                pair("", "b@example.org")
            ]
        );
    }

    #[test]
    fn groups_contribute_their_members_only() {
        assert_eq!(
            who("undisclosed-recipients:;"),
            Vec::<(String, String)>::new()
        );
        assert_eq!(
            who("Team: a@example.org, B <b@example.org>; c@example.org"),
            [
                pair("", "a@example.org"),
                pair("B", "b@example.org"),
                pair("", "c@example.org")
            ]
        );
        assert_eq!(
            who("x@example.org, Friends: y@example.org;"),
            [pair("", "x@example.org"), pair("", "y@example.org")]
        );
    }

    #[test]
    fn entries_without_a_usable_address_are_skipped() {
        assert_eq!(who(""), Vec::<(String, String)>::new());
        assert_eq!(who("   ,  ; "), Vec::<(String, String)>::new());
        assert_eq!(who("Max Muster"), Vec::<(String, String)>::new());
        assert_eq!(who("Max <>"), Vec::<(String, String)>::new());
        assert_eq!(who("Max <not an address>"), Vec::<(String, String)>::new());
        assert_eq!(who("<a@b.example"), Vec::<(String, String)>::new());
        assert_eq!(
            who("Max Muster max@example.org"),
            [pair("Max Muster", "max@example.org")]
        );
        assert_eq!(
            who("broken, ok@example.org, @@@@ <x@y@z>, <@x.example>, <x@>, stray> <s@x.example>"),
            [pair("", "ok@example.org"), pair("stray>", "s@x.example")]
        );
    }

    #[test]
    fn address_lists_are_bounded() {
        let many = (0..500)
            .map(|n| format!("u{n}@example.org"))
            .collect::<Vec<_>>()
            .join(", ");
        let found = who(&many);
        assert_eq!(found.len(), MAX_PEOPLE);
        assert_eq!(found.first(), Some(&pair("", "u0@example.org")));
        let long_local = format!("{}@example.org", "a".repeat(300));
        assert_eq!(who(&long_local), Vec::<(String, String)>::new());
        let long_name = format!("{} <a@example.org>", "n".repeat(1000));
        let found = who(&long_name);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found.first().map(|(name, _)| name.chars().count()),
            Some(MAX_NAME_CHARS + 6)
        );
        let huge = format!("<{}", "(".repeat(100_000));
        assert!(people(&huge).is_empty());
        let quotes = "\"".repeat(100_000);
        assert!(people(&quotes).is_empty());
    }

    #[test]
    fn hostile_names_are_cleaned() {
        assert_eq!(
            who("\"Evil\u{202e}\r\nBcc: x@example.org\" <e@example.org>"),
            [pair("Evil Bcc: x@example.org", "e@example.org")]
        );
        assert_eq!(
            who("a\u{0}b <e@example.org>"),
            [pair("ab", "e@example.org")]
        );
        assert_eq!(who("<e@exa\u{0}mple.org>"), Vec::<(String, String)>::new());
        assert_eq!(who("<e@exa mple.org>"), Vec::<(String, String)>::new());
    }

    #[test]
    fn message_id_tokens_are_checked_for_shape() {
        assert!(message_id_ok("<a@x.example>"));
        assert!(message_id_ok("<a>"));
        for bad in [
            "",
            "a@x.example",
            "<>",
            "<a@x.example",
            "a@x.example>",
            "<a b>",
            "<a\r\nb>",
            "<a<b>",
            "<\u{e4}>",
        ] {
            assert!(!message_id_ok(bad), "{bad:?}");
        }
        assert!(!message_id_ok(&format!(
            "<{}>",
            "a".repeat(MAX_ID_CHARS + 1)
        )));
    }

    #[test]
    fn message_ids_are_collected_and_sanitised() {
        assert_eq!(
            message_ids("<a@x.example> <b@y.example>\r\n\t<c@z.example>"),
            ["<a@x.example>", "<b@y.example>", "<c@z.example>"]
        );
        assert_eq!(message_ids(" <only@x.example> "), ["<only@x.example>"]);
        assert_eq!(message_ids("no ids here"), Vec::<String>::new());
        assert_eq!(message_ids(""), Vec::<String>::new());
        assert_eq!(message_ids("<>"), Vec::<String>::new());
        assert_eq!(
            message_ids("<a b@x.example> <ok@x.example>"),
            ["<ok@x.example>"]
        );
        assert_eq!(
            message_ids("<a\r\nBcc: x@example.org> <ok@x.example>"),
            ["<ok@x.example>"]
        );
        assert_eq!(
            message_ids("<a<b@x.example> <ok@x.example>"),
            ["<ok@x.example>"]
        );
        assert_eq!(message_ids("<ünï@x.example>"), Vec::<String>::new());
        assert_eq!(message_ids("<unterminated@x.example"), Vec::<String>::new());
        let long = format!("<{}@x.example>", "a".repeat(MAX_ID_CHARS));
        assert!(message_ids(&long).is_empty());
        let many = "<a@x.example>".repeat(1000);
        assert_eq!(message_ids(&many).len(), MAX_IDS);
    }
}
