//! Text sanitizing and the argument conversions of the tools.
//!
//! Mail text is untrusted: invisible and control characters are stripped before
//! anything reaches the model or a dialog. All lengths count Unicode code points,
//! like Python's `len()`.

use crate::error::{ToolError, ToolResult};
use crate::json::Json;
use crate::pycompat;

fn is_control(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}' | '\u{7f}')
}

/// Zero-width, bidirectional-control, soft-hyphen and byte-order-mark characters.
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{feff}'
            | '\u{ad}'
    )
}

/// Number of code points.
pub fn char_len(text: &str) -> usize {
    text.chars().count()
}

/// The first `count` code points.
pub fn take_chars(text: &str, count: usize) -> String {
    text.chars().take(count).collect()
}

/// Sanitizes text: removes control and invisible characters, normalizes line ends
/// to `\n`, removes blanks before a line end, allows at most one empty line in a
/// row, trims the text and cuts it at `limit` characters (marked with `[...]`).
pub fn clean_str(text: &str, limit: Option<usize>) -> String {
    let kept: String = text
        .chars()
        .filter(|c| !is_control(*c) && !is_invisible(*c))
        .collect();
    let kept = kept.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(kept.len());
    for c in kept.chars() {
        if c == '\n' {
            while out.ends_with([' ', '\t']) {
                out.pop();
            }
            let trailing = out.chars().rev().take_while(|x| *x == '\n').count();
            if trailing < 2 {
                out.push('\n');
            }
        } else {
            out.push(c);
        }
    }
    let mut text = pycompat::strip(&out).to_string();
    if let Some(limit) = limit {
        if char_len(&text) > limit {
            let cut = take_chars(&text, limit);
            text = format!("{} [...]", pycompat::rstrip(&cut));
        }
    }
    text
}

/// [`clean_str`] of a JSON value (`str(value)`, null and missing become empty).
pub fn clean(value: Option<&Json>, limit: Option<usize>) -> String {
    clean_str(&pycompat::text_of(value), limit)
}

/// Collapses all whitespace (including line breaks) into single blanks, then sanitizes.
pub fn one_line_str(text: &str, limit: usize) -> String {
    let mut collapsed = String::with_capacity(text.len());
    let mut in_space = false;
    for c in text.chars() {
        if pycompat::is_space(c) {
            if !in_space {
                collapsed.push(' ');
            }
            in_space = true;
        } else {
            collapsed.push(c);
            in_space = false;
        }
    }
    clean_str(&collapsed, Some(limit))
}

/// [`one_line_str`] of a JSON value.
pub fn one_line(value: Option<&Json>, limit: usize) -> String {
    one_line_str(&pycompat::text_of(value), limit)
}

fn invalid_number(value: &Json) -> ToolError {
    ToolError::msg(format!("Invalid number: {}", pycompat::repr(value)))
}

/// A number argument clamped to `low..=high`; missing, null and `""` give `None`.
pub fn to_int_opt(value: Option<&Json>, low: i64, high: i64) -> ToolResult<Option<i64>> {
    let value = match value {
        None | Some(Json::Null) => return Ok(None),
        Some(Json::Str(text)) if text.is_empty() => return Ok(None),
        Some(other) => other,
    };
    if matches!(value, Json::Bool(_)) {
        return Err(invalid_number(value));
    }
    let Some(number) = pycompat::parse_float(&pycompat::str_of(value)) else {
        return Err(invalid_number(value));
    };
    if number.is_nan() {
        return Err(invalid_number(value));
    }
    if number.is_infinite() {
        // Python: OverflowError from int(float('inf')), which is not a ToolError.
        return Err(ToolError::internal("OverflowError"));
    }
    let clamped = number.trunc().clamp(low as f64, high as f64);
    Ok(Some(clamped as i64))
}

/// [`to_int_opt`] with a default for a missing value.
pub fn to_int(value: Option<&Json>, default: i64, low: i64, high: i64) -> ToolResult<i64> {
    Ok(to_int_opt(value, low, high)?.unwrap_or(default))
}

/// A flag argument: booleans as they are, strings `1`, `true`, `yes`, `ja`, `y` are true.
pub fn to_bool(value: Option<&Json>) -> bool {
    match value {
        None | Some(Json::Null) => false,
        Some(Json::Str(text)) if text.is_empty() => false,
        Some(Json::Bool(flag)) => *flag,
        Some(other) => {
            let text = pycompat::lower(pycompat::strip(&pycompat::str_of(other)));
            matches!(text.as_str(), "1" | "true" | "yes" | "ja" | "y")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn j(text: &str) -> Json {
        crate::json::parse(text).expect("valid json")
    }

    #[test]
    fn clean_strips_control_and_invisible_characters() {
        assert_eq!(clean_str("a\u{0}b\u{7}c\u{7f}d\u{1b}e", None), "abcde");
        assert_eq!(
            clean_str("pa\u{200b}ss\u{202e}wo\u{2060}rd\u{feff}\u{ad}!", None),
            "password!"
        );
        assert_eq!(clean_str("x\u{2066}y\u{2069}z\u{200f}", None), "xyz");
    }

    #[test]
    fn clean_keeps_tabs_newlines_and_other_whitespace_characters() {
        assert_eq!(clean_str("a\tb\nc", None), "a\tb\nc");
        // NEL and the line separator are not control characters for the filter.
        assert_eq!(clean_str("a\u{85}b\u{2028}c", None), "a\u{85}b\u{2028}c");
    }

    #[test]
    fn clean_normalizes_line_ends_and_blank_lines() {
        assert_eq!(clean_str("a\r\nb\rc", None), "a\nb\nc");
        assert_eq!(clean_str("a  \t\nb", None), "a\nb");
        assert_eq!(clean_str("a\n\n\n\n\nb", None), "a\n\nb");
        assert_eq!(clean_str("a\n \n\n \nb", None), "a\n\nb");
        assert_eq!(clean_str("a\n\n\n \nb", None), "a\n\nb");
        assert_eq!(clean_str("  \n a b \n\n ", None), "a b");
        assert_eq!(clean_str("a \u{a0}\nb", None), "a \u{a0}\nb");
    }

    #[test]
    fn clean_trims_python_whitespace_and_cuts_at_the_limit() {
        assert_eq!(clean_str("\u{a0}\u{3000}x\u{2028}", None), "x");
        assert_eq!(clean_str("abcdef", Some(6)), "abcdef");
        assert_eq!(clean_str("abcdefg", Some(6)), "abcdef [...]");
        assert_eq!(clean_str("abc   fg", Some(6)), "abc [...]");
        assert_eq!(
            clean_str("\u{e4}\u{20ac}\u{1f600}xyz", Some(3)),
            "\u{e4}\u{20ac}\u{1f600} [...]"
        );
    }

    #[test]
    fn clean_of_values_follows_python_str() {
        assert_eq!(clean(None, None), "");
        assert_eq!(clean(Some(&Json::Null), None), "");
        assert_eq!(clean(Some(&j("5")), None), "5");
        assert_eq!(clean(Some(&j("true")), None), "True");
        assert_eq!(clean(Some(&j("[\"a\", 1]")), None), "['a', 1]");
        assert_eq!(clean(Some(&j("\"  hi \"")), Some(10)), "hi");
    }

    #[test]
    fn one_line_collapses_whitespace() {
        assert_eq!(one_line_str("a\r\nb\t c\u{a0}\u{2028}d", 50), "a b c d");
        assert_eq!(one_line_str("  x  ", 50), "x");
        assert_eq!(
            one_line_str("Hallo\r\nBcc: evil@example.org", 200),
            "Hallo Bcc: evil@example.org"
        );
        assert_eq!(one_line_str(&"a ".repeat(200), 10), "a a a a a [...]");
        assert_eq!(one_line(None, 10), "");
    }

    #[test]
    fn to_int_converts_clamps_and_defaults() {
        assert_eq!(to_int(None, 3, 1, 365), Ok(3));
        assert_eq!(to_int(Some(&Json::Null), 3, 1, 365), Ok(3));
        assert_eq!(to_int(Some(&j("\"\"")), 3, 1, 365), Ok(3));
        assert_eq!(to_int(Some(&j("7")), 3, 1, 365), Ok(7));
        assert_eq!(to_int(Some(&j("\"7\"")), 3, 1, 365), Ok(7));
        assert_eq!(to_int(Some(&j("\" 7.9 \"")), 3, 1, 365), Ok(7));
        assert_eq!(to_int(Some(&j("-7.9")), 3, -100, 365), Ok(-7));
        assert_eq!(to_int(Some(&j("0")), 3, 1, 365), Ok(1));
        assert_eq!(to_int(Some(&j("100000")), 3, 1, 365), Ok(365));
        assert_eq!(to_int(Some(&j("1e3")), 3, 1, 365), Ok(365));
        assert_eq!(
            to_int(Some(&j("123456789012345678901234567890")), 3, 1, 50),
            Ok(50)
        );
        assert_eq!(to_int(Some(&j("\"1_0\"")), 3, 1, 365), Ok(10));
        assert_eq!(to_int_opt(None, 1, 5), Ok(None));
    }

    #[test]
    fn to_int_errors_name_the_value_like_python() {
        let message = |value: &str| match to_int(Some(&j(value)), 3, 1, 5) {
            Err(ToolError::Msg(text)) => text,
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(message("true"), "Invalid number: True");
        assert_eq!(message("false"), "Invalid number: False");
        assert_eq!(message("\"abc\""), "Invalid number: 'abc'");
        assert_eq!(message("\"it's\""), "Invalid number: \"it's\"");
        assert_eq!(message("[1]"), "Invalid number: [1]");
        assert_eq!(message("{\"a\": 1}"), "Invalid number: {'a': 1}");
        assert_eq!(message("\"nan\""), "Invalid number: 'nan'");
        assert_eq!(message("\" \""), "Invalid number: ' '");
    }

    #[test]
    fn to_int_of_infinity_is_an_internal_error_like_python() {
        assert_eq!(
            to_int(Some(&j("\"inf\"")), 3, 1, 5),
            Err(ToolError::Internal("OverflowError".into()))
        );
        let huge = Json::BigInt("1".to_string() + &"0".repeat(400));
        assert_eq!(
            to_int(Some(&huge), 3, 1, 5),
            Err(ToolError::Internal("OverflowError".into()))
        );
    }

    #[test]
    fn to_bool_follows_python() {
        assert!(!to_bool(None));
        assert!(!to_bool(Some(&Json::Null)));
        assert!(!to_bool(Some(&j("\"\""))));
        assert!(to_bool(Some(&j("true"))));
        assert!(!to_bool(Some(&j("false"))));
        for yes in [
            "\"1\"",
            "\"true\"",
            "\" TRUE \"",
            "\"yes\"",
            "\"ja\"",
            "\"Y\"",
            "1",
        ] {
            assert!(to_bool(Some(&j(yes))), "{yes}");
        }
        for no in [
            "\"0\"", "\"no\"", "\"nein\"", "0", "2", "1.0", "[]", "[1]", "\"x\"",
        ] {
            assert!(!to_bool(Some(&j(no))), "{no}");
        }
    }

    #[test]
    fn counts_code_points_not_bytes() {
        assert_eq!(char_len("\u{e4}\u{1f600}x"), 3);
        assert_eq!(take_chars("\u{e4}\u{1f600}xyz", 2), "\u{e4}\u{1f600}");
    }
}
