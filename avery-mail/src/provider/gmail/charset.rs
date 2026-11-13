const WINDOWS_1252: [char; 32] = [
    '\u{20AC}', '\u{FFFD}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{FFFD}', '\u{017D}', '\u{FFFD}',
    '\u{FFFD}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}', '\u{FFFD}', '\u{017E}', '\u{0178}',
];

const LATIN9: [(u8, char); 8] = [
    (0xA4, '\u{20AC}'),
    (0xA6, '\u{0160}'),
    (0xA8, '\u{0161}'),
    (0xB4, '\u{017D}'),
    (0xB8, '\u{017E}'),
    (0xBC, '\u{0152}'),
    (0xBD, '\u{0153}'),
    (0xBE, '\u{0178}'),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Charset {
    Utf8,
    Windows1252,
    Latin9,
    Unlabelled,
}

fn charset_of(label: &str) -> Charset {
    let label = label.split('*').next().unwrap_or_default();
    let label = label
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .trim()
        .to_ascii_lowercase();
    match label.as_str() {
        "utf-8" | "utf8" => Charset::Utf8,
        "windows-1252" | "cp1252" | "x-cp1252" | "iso-8859-1" | "iso8859-1" | "iso_8859-1"
        | "latin1" | "latin-1" | "l1" => Charset::Windows1252,
        "iso-8859-15" | "iso8859-15" | "iso_8859-15" | "latin9" | "latin-9" | "l9" => {
            Charset::Latin9
        }
        _ => Charset::Unlabelled,
    }
}

fn single_byte(byte: u8, latin9: bool) -> char {
    if latin9 {
        if let Some((_, replacement)) = LATIN9.iter().find(|(code, _)| *code == byte) {
            return *replacement;
        }
    }
    match byte {
        0x80..=0x9F => WINDOWS_1252
            .get(usize::from(byte - 0x80))
            .copied()
            .unwrap_or('\u{FFFD}'),
        other => char::from(other),
    }
}

fn single_bytes(bytes: &[u8], latin9: bool) -> String {
    bytes
        .iter()
        .map(|byte| single_byte(*byte, latin9))
        .collect()
}

pub(crate) fn decode(label: &str, bytes: &[u8]) -> String {
    match charset_of(label) {
        Charset::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
        Charset::Windows1252 => single_bytes(bytes, false),
        Charset::Latin9 => single_bytes(bytes, true),
        Charset::Unlabelled => match std::str::from_utf8(bytes) {
            Ok(text) => text.to_string(),
            Err(_) => single_bytes(bytes, false),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_is_decoded_and_invalid_bytes_become_replacement_characters() {
        assert_eq!(decode("UTF-8", "Grüße €".as_bytes()), "Grüße €");
        assert_eq!(decode("utf8", b"abc"), "abc");
        assert_eq!(decode("\"utf-8\"", "ä".as_bytes()), "ä");
        assert_eq!(decode("utf-8*de", "ä".as_bytes()), "ä");
        assert_eq!(decode("utf-8", &[b'a', 0xFF, b'b']), "a\u{FFFD}b");
        assert_eq!(decode("utf-8", &[0xE2, 0x82]), "\u{FFFD}");
        assert_eq!(decode("utf-8", &[]), "");
    }

    #[test]
    fn latin1_and_windows_1252_share_one_table() {
        assert_eq!(decode("ISO-8859-1", &[0x4B, 0xE4, 0x73, 0x65]), "Käse");
        assert_eq!(decode("latin1", &[0xFC]), "ü");
        assert_eq!(
            decode("windows-1252", &[0x80, 0x93, 0x94, 0x96, 0x99]),
            "€“”–™"
        );
        assert_eq!(decode("cp1252", &[0x85]), "…");
        assert_eq!(
            decode("iso-8859-1", &[0x81, 0x8D, 0x8F, 0x90, 0x9D]),
            "\u{FFFD}".repeat(5)
        );
        assert_eq!(decode("iso-8859-1", &[0xFF, 0xA0]), "ÿ\u{A0}");
    }

    #[test]
    fn latin9_differs_from_latin1_in_eight_places() {
        assert_eq!(decode("ISO-8859-15", &[0xA4, 0xA6, 0xA8, 0xB4]), "€ŠšŽ");
        assert_eq!(decode("iso-8859-15", &[0xB8, 0xBC, 0xBD, 0xBE]), "žŒœŸ");
        assert_eq!(decode("iso-8859-15", &[0xE4, 0x41]), "äA");
    }

    #[test]
    fn unlabelled_or_unknown_charsets_try_utf8_before_windows_1252() {
        for label in [
            "",
            "us-ascii",
            "ascii",
            "x-unknown",
            "iso-8859-2",
            "koi8-r",
            "  ",
        ] {
            assert_eq!(decode(label, "Grüße".as_bytes()), "Grüße", "{label:?}");
            assert_eq!(
                decode(label, &[0x47, 0x72, 0xFC, 0xDF, 0x65]),
                "Grüße",
                "{label:?}"
            );
        }
        assert_eq!(decode("us-ascii", b"plain"), "plain");
    }

    #[test]
    fn labels_are_matched_without_case_quotes_or_language_suffix() {
        assert_eq!(charset_of("  UTF-8  "), Charset::Utf8);
        assert_eq!(charset_of("'iso-8859-1'"), Charset::Windows1252);
        assert_eq!(charset_of("ISO-8859-15*en"), Charset::Latin9);
        assert_eq!(charset_of("*"), Charset::Unlabelled);
        assert_eq!(charset_of("utf-16"), Charset::Unlabelled);
    }
}
