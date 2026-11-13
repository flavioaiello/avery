use crate::error::{ToolError, ToolResult};
use crate::json::{self, Json};

fn is_control(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}' | '\u{7f}')
}

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

pub(crate) fn char_len(text: &str) -> usize {
    text.chars().count()
}

pub(crate) fn take_chars(text: &str, count: usize) -> String {
    text.chars().take(count).collect()
}

pub(crate) fn clean_str(text: &str, limit: Option<usize>) -> String {
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
    let mut text = out.trim().to_string();
    if let Some(limit) = limit {
        if char_len(&text) > limit {
            let cut = take_chars(&text, limit);
            text = format!("{} [...]", cut.trim_end());
        }
    }
    text
}

pub(crate) fn text_of(value: Option<&Json>) -> String {
    match value {
        Some(Json::Str(text)) => text.clone(),
        Some(Json::Int(number)) => number.to_string(),
        Some(Json::BigInt(digits)) => digits.clone(),
        Some(Json::Float(number)) if number.is_finite() => number.to_string(),
        Some(Json::Bool(flag)) => flag.to_string(),
        _ => String::new(),
    }
}

pub(crate) fn clean(value: Option<&Json>, limit: Option<usize>) -> String {
    clean_str(&text_of(value), limit)
}

pub(crate) fn one_line_str(text: &str, limit: usize) -> String {
    let mut collapsed = String::with_capacity(text.len());
    let mut in_space = false;
    for c in text.chars() {
        if c.is_whitespace() {
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

pub(crate) fn one_line(value: Option<&Json>, limit: usize) -> String {
    one_line_str(&text_of(value), limit)
}

pub(crate) fn bracketed(text: &str) -> Option<&str> {
    let mut start = 0;
    while let Some(offset) = text.get(start..)?.find('<') {
        let inner_start = start + offset + 1;
        let rest = text.get(inner_start..)?;
        let inner_len = rest.find(['<', '>']).unwrap_or(rest.len());
        if inner_len > 0
            && rest
                .get(inner_len..)
                .is_some_and(|tail| tail.starts_with('>'))
        {
            return rest.get(..inner_len);
        }
        start = inner_start;
    }
    None
}

fn invalid_number(value: &Json) -> ToolError {
    let shown = match value {
        Json::Str(text) => format!("\"{}\"", clean_str(text, Some(40))),
        Json::Arr(_) => "an array".to_string(),
        Json::Obj(_) => "an object".to_string(),
        other => json::dumps(other, true),
    };
    ToolError::msg(format!("Invalid number: {shown}"))
}

fn whole(number: f64) -> Option<i64> {
    if !number.is_finite() {
        return None;
    }
    let digits = format!("{:.0}", number.trunc());
    Some(
        digits
            .parse::<i64>()
            .unwrap_or(if number < 0.0 { i64::MIN } else { i64::MAX }),
    )
}

pub(crate) fn to_int_opt(value: Option<&Json>, low: i64, high: i64) -> ToolResult<Option<i64>> {
    let value = match value {
        None | Some(Json::Null) => return Ok(None),
        Some(Json::Str(text)) if text.is_empty() => return Ok(None),
        Some(other) => other,
    };
    let number = match value {
        Json::Int(number) => *number,
        Json::BigInt(digits) => {
            if digits.starts_with('-') {
                i64::MIN
            } else {
                i64::MAX
            }
        }
        Json::Float(number) => whole(*number).ok_or_else(|| invalid_number(value))?,
        Json::Str(text) => {
            let text = text.trim();
            match text.parse::<i64>() {
                Ok(number) => number,
                Err(_) => text
                    .parse::<f64>()
                    .ok()
                    .and_then(whole)
                    .ok_or_else(|| invalid_number(value))?,
            }
        }
        _ => return Err(invalid_number(value)),
    };
    Ok(Some(number.clamp(low, high)))
}

pub(crate) fn to_int(value: Option<&Json>, default: i64, low: i64, high: i64) -> ToolResult<i64> {
    Ok(to_int_opt(value, low, high)?.unwrap_or(default))
}

pub(crate) fn to_bool(value: Option<&Json>) -> bool {
    match value {
        Some(Json::Bool(flag)) => *flag,
        other => matches!(
            text_of(other).trim().to_lowercase().as_str(),
            "1" | "true" | "yes" | "ja" | "y"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{j, TestResult};

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
    fn clean_trims_unicode_whitespace_and_cuts_at_the_limit() {
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
    fn clean_of_values_renders_scalars_and_ignores_containers() -> TestResult {
        assert_eq!(clean(None, None), "");
        assert_eq!(clean(Some(&Json::Null), None), "");
        assert_eq!(clean(Some(&j("5")?), None), "5");
        assert_eq!(clean(Some(&j("true")?), None), "true");
        assert_eq!(clean(Some(&j("1.5")?), None), "1.5");
        assert_eq!(
            clean(Some(&j("123456789012345678901234567890")?), None),
            "123456789012345678901234567890"
        );
        assert_eq!(clean(Some(&j("[\"a\", 1]")?), None), "");
        assert_eq!(clean(Some(&j("{\"a\": 1}")?), None), "");
        assert_eq!(clean(Some(&j("\"  hi \"")?), Some(10)), "hi");
        Ok(())
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
    fn to_int_converts_clamps_and_defaults() -> TestResult {
        assert_eq!(to_int(None, 3, 1, 365), Ok(3));
        assert_eq!(to_int(Some(&Json::Null), 3, 1, 365), Ok(3));
        assert_eq!(to_int(Some(&j("\"\"")?), 3, 1, 365), Ok(3));
        assert_eq!(to_int(Some(&j("7")?), 3, 1, 365), Ok(7));
        assert_eq!(to_int(Some(&j("\"7\"")?), 3, 1, 365), Ok(7));
        assert_eq!(to_int(Some(&j("\" 7.9 \"")?), 3, 1, 365), Ok(7));
        assert_eq!(to_int(Some(&j("-7.9")?), 3, -100, 365), Ok(-7));
        assert_eq!(to_int(Some(&j("0")?), 3, 1, 365), Ok(1));
        assert_eq!(to_int(Some(&j("100000")?), 3, 1, 365), Ok(365));
        assert_eq!(to_int(Some(&j("1e3")?), 3, 1, 365), Ok(365));
        assert_eq!(to_int(Some(&j("-1e3")?), 3, 1, 365), Ok(1));
        assert_eq!(to_int(Some(&j("\"1e30\"")?), 3, 1, 365), Ok(365));
        assert_eq!(to_int(Some(&j("\"-1e30\"")?), 3, 1, 365), Ok(1));
        assert_eq!(
            to_int(Some(&j("123456789012345678901234567890")?), 3, 1, 50),
            Ok(50)
        );
        assert_eq!(
            to_int(Some(&j("-123456789012345678901234567890")?), 3, 1, 50),
            Ok(1)
        );
        assert_eq!(
            to_int(
                Some(&Json::BigInt("1".to_string() + &"0".repeat(400))),
                3,
                1,
                5
            ),
            Ok(5)
        );
        assert_eq!(to_int_opt(None, 1, 5), Ok(None));
        Ok(())
    }

    #[test]
    fn to_int_rejects_what_is_not_a_number() -> TestResult {
        let invalid = |text: &str| ToolError::msg(format!("Invalid number: {text}"));
        assert_eq!(to_int(Some(&j("true")?), 3, 1, 5), Err(invalid("true")));
        assert_eq!(to_int(Some(&j("false")?), 3, 1, 5), Err(invalid("false")));
        assert_eq!(
            to_int(Some(&j("\"abc\"")?), 3, 1, 5),
            Err(invalid("\"abc\""))
        );
        assert_eq!(to_int(Some(&j("[1]")?), 3, 1, 5), Err(invalid("an array")));
        assert_eq!(
            to_int(Some(&j("{\"a\": 1}")?), 3, 1, 5),
            Err(invalid("an object"))
        );
        assert_eq!(
            to_int(Some(&j("\"nan\"")?), 3, 1, 5),
            Err(invalid("\"nan\""))
        );
        assert_eq!(
            to_int(Some(&j("\"inf\"")?), 3, 1, 5),
            Err(invalid("\"inf\""))
        );
        assert_eq!(
            to_int(Some(&j("\"1e400\"")?), 3, 1, 5),
            Err(invalid("\"1e400\""))
        );
        assert_eq!(
            to_int(Some(&j("\"1_0\"")?), 3, 1, 5),
            Err(invalid("\"1_0\""))
        );
        assert_eq!(to_int(Some(&j("\" \"")?), 3, 1, 5), Err(invalid("\"\"")));
        Ok(())
    }

    #[test]
    fn to_int_errors_stay_short_for_long_input() {
        let long = Json::Str("x".repeat(100_000));
        let message = to_int(Some(&long), 3, 1, 5)
            .err()
            .map(|error| error.message());
        assert!(message.is_some_and(|text| text.chars().count() < 80));
    }

    #[test]
    fn to_bool_accepts_flags_and_a_few_words() -> TestResult {
        assert!(!to_bool(None));
        assert!(!to_bool(Some(&Json::Null)));
        assert!(!to_bool(Some(&j("\"\"")?)));
        assert!(to_bool(Some(&j("true")?)));
        assert!(!to_bool(Some(&j("false")?)));
        for yes in [
            "\"1\"",
            "\"true\"",
            "\" TRUE \"",
            "\"yes\"",
            "\"ja\"",
            "\"Y\"",
            "1",
            "1.0",
        ] {
            assert!(to_bool(Some(&j(yes)?)), "{yes}");
        }
        for no in [
            "\"0\"", "\"no\"", "\"nein\"", "0", "2", "[]", "[1]", "{}", "\"x\"",
        ] {
            assert!(!to_bool(Some(&j(no)?)), "{no}");
        }
        Ok(())
    }

    #[test]
    fn bracketed_finds_the_first_non_empty_group() {
        assert_eq!(
            bracketed("Anna <anna@example.org>"),
            Some("anna@example.org")
        );
        assert_eq!(bracketed("plain@example.org"), None);
        assert_eq!(bracketed("<>"), None);
        assert_eq!(bracketed("a <> b <x@y.z> <q>"), Some("x@y.z"));
        assert_eq!(bracketed("<a<b>"), Some("b"));
        assert_eq!(bracketed("<a"), None);
        assert_eq!(bracketed("a>b"), None);
        assert_eq!(bracketed("<\u{e4}\u{20ac}> x"), Some("\u{e4}\u{20ac}"));
    }

    #[test]
    fn counts_code_points_not_bytes() {
        assert_eq!(char_len("\u{e4}\u{1f600}x"), 3);
        assert_eq!(take_chars("\u{e4}\u{1f600}xyz", 2), "\u{e4}\u{1f600}");
    }
}
