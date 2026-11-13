//! Helpers that reproduce the behaviour of Python 3.9 built-ins the reference
//! implementation relies on: whitespace rules, `repr`/`str` of JSON values,
//! `float()`, and the float formatting of `json.dumps`.

use std::fmt::Write as _;

use crate::json::Json;

/// `str.isspace()` for one character. This is also what `str.strip()` removes and
/// what the regular expression class `\s` matches in Python 3.
pub fn is_space(c: char) -> bool {
    matches!(
        c,
        '\u{9}'..='\u{d}'
            | '\u{1c}'..='\u{20}'
            | '\u{85}'
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
    )
}

/// `str.strip()`.
pub fn strip(text: &str) -> &str {
    text.trim_matches(is_space)
}

/// `str.rstrip()`.
pub fn rstrip(text: &str) -> &str {
    text.trim_end_matches(is_space)
}

/// `str.lower()`.
pub fn lower(text: &str) -> String {
    text.to_lowercase()
}

/// `repr(float)`: the shortest text that reads back as the same value, in
/// positional notation for exponents from -4 up to 15, otherwise in scientific
/// notation with at least two exponent digits (`1e+16`, `1e-05`).
pub fn float_repr(value: f64) -> String {
    if value.is_nan() {
        return "nan".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        }
        .to_string();
    }
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let count = i32::try_from(digits.len()).unwrap_or(i32::MAX);
    let point = exponent + 1;
    let mut out = String::new();
    if value < 0.0 {
        out.push('-');
    }
    if point <= -4 || point > 16 {
        let (first, rest) = digits.split_at(1.min(digits.len()));
        out.push_str(first);
        if !rest.is_empty() {
            out.push('.');
            out.push_str(rest);
        }
        let shown = point - 1;
        let _ = write!(
            out,
            "e{}{:02}",
            if shown < 0 { '-' } else { '+' },
            shown.unsigned_abs()
        );
    } else if point <= 0 {
        out.push_str("0.");
        for _ in 0..point.unsigned_abs() {
            out.push('0');
        }
        out.push_str(&digits);
    } else if point >= count {
        out.push_str(&digits);
        for _ in 0..(point - count) {
            out.push('0');
        }
        out.push_str(".0");
    } else {
        let (whole, fraction) = digits.split_at(usize::try_from(point).unwrap_or(0));
        out.push_str(whole);
        out.push('.');
        out.push_str(fraction);
    }
    out
}

/// Whether Python's `str.isprintable()` would accept the character. The table
/// covers the categories Cc, Cf, Co, Zl, Zp and Zs and the noncharacters; other
/// unassigned code points are treated as printable.
fn is_printable(c: char) -> bool {
    let code = u32::from(c);
    if code < 0x80 {
        return (0x20..0x7f).contains(&code);
    }
    !matches!(
        code,
        0x80..=0xa0
            | 0xad
            | 0x600..=0x605
            | 0x61c
            | 0x6dd
            | 0x70f
            | 0x8e2
            | 0x1680
            | 0x180e
            | 0x2000..=0x200f
            | 0x2028..=0x202f
            | 0x205f..=0x2064
            | 0x2066..=0x206f
            | 0x3000
            | 0xe000..=0xf8ff
            | 0xfdd0..=0xfdef
            | 0xfeff
            | 0xfff9..=0xfffb
            | 0xfffe..=0xffff
            | 0x110bd
            | 0x110cd
            | 0x13430..=0x13438
            | 0x1bca0..=0x1bca3
            | 0x1d173..=0x1d17a
            | 0xe0001
            | 0xe0020..=0xe007f
            | 0xf0000..=0x10ffff
    ) && !(code & 0xfffe == 0xfffe)
        && !(0x40000..=0xdffff).contains(&code)
}

/// `repr(str)`.
pub fn repr_str(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::new();
    out.push(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if is_printable(c) => out.push(c),
            c => {
                let code = u32::from(c);
                if code <= 0xff {
                    let _ = write!(out, "\\x{code:02x}");
                } else if code <= 0xffff {
                    let _ = write!(out, "\\u{code:04x}");
                } else {
                    let _ = write!(out, "\\U{code:08x}");
                }
            }
        }
    }
    out.push(quote);
    out
}

/// `repr(value)` of a value decoded from JSON.
pub fn repr(value: &Json) -> String {
    match value {
        Json::Null => "None".to_string(),
        Json::Bool(true) => "True".to_string(),
        Json::Bool(false) => "False".to_string(),
        Json::Int(number) => number.to_string(),
        Json::BigInt(text) => text.clone(),
        Json::Float(number) => float_repr(*number),
        Json::Str(text) => repr_str(text),
        Json::Arr(items) => {
            let parts: Vec<String> = items.iter().map(repr).collect();
            format!("[{}]", parts.join(", "))
        }
        Json::Obj(object) => {
            let parts: Vec<String> = object
                .iter()
                .map(|(key, item)| format!("{}: {}", repr_str(key), repr(item)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
    }
}

/// `str(value)` of a value decoded from JSON.
pub fn str_of(value: &Json) -> String {
    match value {
        Json::Str(text) => text.clone(),
        other => repr(other),
    }
}

/// `str(value)` where a missing value or null is the empty string (the Python
/// code often spells this `"" if value is None else str(value)`).
pub fn text_of(value: Option<&Json>) -> String {
    match value {
        None | Some(Json::Null) => String::new(),
        Some(other) => str_of(other),
    }
}

/// `float(text)`: surrounding whitespace is ignored, single underscores between
/// digits are allowed, and `inf`, `infinity` and `nan` are accepted.
pub fn parse_float(text: &str) -> Option<f64> {
    let text = strip(text);
    let chars: Vec<char> = text.chars().collect();
    let mut cleaned = String::with_capacity(text.len());
    for (position, &c) in chars.iter().enumerate() {
        if c == '_' {
            let before = position.checked_sub(1).and_then(|p| chars.get(p));
            let after = chars.get(position + 1);
            if !before.is_some_and(char::is_ascii_digit) || !after.is_some_and(char::is_ascii_digit)
            {
                return None;
            }
        } else {
            cleaned.push(c);
        }
    }
    if cleaned.is_empty() {
        return None;
    }
    cleaned.parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::parse;

    fn value(text: &str) -> Json {
        parse(text).expect("valid json")
    }

    #[test]
    fn whitespace_is_pythons() {
        for c in [
            '\t', '\n', '\u{b}', '\u{c}', '\r', '\u{1c}', '\u{1f}', ' ', '\u{85}', '\u{a0}',
            '\u{2028}', '\u{3000}',
        ] {
            assert!(is_space(c), "{c:?}");
        }
        for c in ['a', '\u{0}', '\u{200b}', '\u{feff}', '\u{180e}', '-'] {
            assert!(!is_space(c), "{c:?}");
        }
        assert_eq!(strip("\u{a0} x y \u{1f}\n"), "x y");
        assert_eq!(rstrip(" x \n"), " x");
    }

    #[test]
    fn float_repr_matches_python() {
        let cases: [(f64, &str); 20] = [
            (1.0, "1.0"),
            (0.5, "0.5"),
            (100.0, "100.0"),
            (1.5e-7, "1.5e-07"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (123456789.123, "123456789.123"),
            (1e15, "1000000000000000.0"),
            (1234567890123456.0, "1234567890123456.0"),
            (1e16, "1e+16"),
            (1.2345678901234568e17, "1.2345678901234568e+17"),
            (1e22, "1e+22"),
            (1e100, "1e+100"),
            (5e-324, "5e-324"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            (-2.5, "-2.5"),
            (0.1 + 0.2, "0.30000000000000004"),
            (3.0e-5, "3e-05"),
            (-0.0, "-0.0"),
            (0.0, "0.0"),
        ];
        for (number, expected) in cases {
            assert_eq!(float_repr(number), expected, "{number:e}");
        }
    }

    #[test]
    fn repr_of_strings_follows_python_quoting() {
        assert_eq!(repr_str("abc"), "'abc'");
        assert_eq!(repr_str("it's"), "\"it's\"");
        assert_eq!(repr_str("say \"hi\""), "'say \"hi\"'");
        assert_eq!(repr_str("both ' and \""), "'both \\' and \"'");
        assert_eq!(repr_str("a\\b\n\t\r"), "'a\\\\b\\n\\t\\r'");
        assert_eq!(repr_str("\u{0}\u{7f}\u{1b}"), "'\\x00\\x7f\\x1b'");
        assert_eq!(repr_str("ä€😀"), "'ä€😀'");
        assert_eq!(
            repr_str("\u{a0}\u{ad}\u{200b}\u{2028}"),
            "'\\xa0\\xad\\u200b\\u2028'"
        );
        assert_eq!(repr_str("\u{e000}"), "'\\ue000'");
        assert_eq!(repr_str("\u{f0000}"), "'\\U000f0000'");
    }

    #[test]
    fn repr_and_str_of_json_values() {
        assert_eq!(repr(&value("null")), "None");
        assert_eq!(repr(&value("true")), "True");
        assert_eq!(repr(&value("false")), "False");
        assert_eq!(repr(&value("12")), "12");
        assert_eq!(repr(&value("1.0")), "1.0");
        assert_eq!(
            repr(&value("[1, \"a\", null, [true]]")),
            "[1, 'a', None, [True]]"
        );
        assert_eq!(
            repr(&value("{\"k\": [1], \"x\": {}}")),
            "{'k': [1], 'x': {}}"
        );
        assert_eq!(str_of(&value("\"text\"")), "text");
        assert_eq!(str_of(&value("[\"text\"]")), "['text']");
        assert_eq!(text_of(None), "");
        assert_eq!(text_of(Some(&Json::Null)), "");
        assert_eq!(text_of(Some(&Json::Int(5))), "5");
    }

    #[test]
    fn float_parsing_follows_python() {
        for (text, expected) in [
            ("5", 5.0),
            (" 5 ", 5.0),
            ("+5", 5.0),
            ("-5.5", -5.5),
            (".5", 0.5),
            ("5.", 5.0),
            ("1e3", 1000.0),
            ("1_000", 1000.0),
            ("1_0.5_5", 10.55),
            ("\u{a0}7\u{3000}", 7.0),
        ] {
            assert_eq!(parse_float(text), Some(expected), "{text:?}");
        }
        assert_eq!(parse_float("inf"), Some(f64::INFINITY));
        assert_eq!(parse_float("-Infinity"), Some(f64::NEG_INFINITY));
        assert!(parse_float("NaN").is_some_and(f64::is_nan));
        for text in [
            "", " ", "abc", "12abc", "1_", "_1", "1__0", "1_.5", "1._5", "0x10", "1e", "e5", ".",
            "--1", "1 2", "[1]",
        ] {
            assert_eq!(parse_float(text), None, "{text:?}");
        }
    }
}
