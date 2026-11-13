use std::collections::HashMap;
use std::fmt;
use std::fmt::Write as _;

pub(crate) const MAX_DEPTH: usize = 128;
pub(crate) const MAX_TEXT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Int(i64),
    BigInt(String),
    Float(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Object),
}

#[derive(Clone, Debug, PartialEq, Default)]
pub(crate) struct Object {
    entries: Vec<(String, Json)>,
}

impl Object {
    pub(crate) fn new() -> Self {
        Object::default()
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn get(&self, key: &str) -> Option<&Json> {
        self.entries
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }

    pub(crate) fn insert(&mut self, key: impl Into<String>, value: impl Into<Json>) {
        let key = key.into();
        let value = value.into();
        match self.entries.iter_mut().find(|(name, _)| *name == key) {
            Some(slot) => slot.1 = value,
            None => self.entries.push((key, value)),
        }
    }

    #[cfg(test)]
    pub(crate) fn remove(&mut self, key: &str) -> Option<Json> {
        let position = self.entries.iter().position(|(name, _)| name == key)?;
        Some(self.entries.remove(position).1)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&str, &Json)> {
        self.entries
            .iter()
            .map(|(key, value)| (key.as_str(), value))
    }

    pub(crate) fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(key, _)| key.as_str())
    }

    fn push_new(&mut self, key: String, value: Json) {
        self.entries.push((key, value));
    }

    fn replace_at(&mut self, index: usize, value: Json) {
        if let Some(slot) = self.entries.get_mut(index) {
            slot.1 = value;
        }
    }
}

impl Json {
    pub(crate) fn object<K: Into<String>, V: Into<Json>>(pairs: Vec<(K, V)>) -> Json {
        let mut object = Object::new();
        for (key, value) in pairs {
            object.insert(key, value);
        }
        Json::Obj(object)
    }

    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(text) => Some(text),
            _ => None,
        }
    }

    pub(crate) fn as_object(&self) -> Option<&Object> {
        match self {
            Json::Obj(object) => Some(object),
            _ => None,
        }
    }

    pub(crate) fn as_array(&self) -> Option<&Vec<Json>> {
        match self {
            Json::Arr(items) => Some(items),
            _ => None,
        }
    }

    pub(crate) fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(flag) => Some(*flag),
            _ => None,
        }
    }
}

impl From<&str> for Json {
    fn from(value: &str) -> Self {
        Json::Str(value.to_string())
    }
}

impl From<String> for Json {
    fn from(value: String) -> Self {
        Json::Str(value)
    }
}

impl From<&String> for Json {
    fn from(value: &String) -> Self {
        Json::Str(value.clone())
    }
}

impl From<bool> for Json {
    fn from(value: bool) -> Self {
        Json::Bool(value)
    }
}

impl From<i64> for Json {
    fn from(value: i64) -> Self {
        Json::Int(value)
    }
}

impl From<usize> for Json {
    fn from(value: usize) -> Self {
        match i64::try_from(value) {
            Ok(number) => Json::Int(number),
            Err(_) => Json::BigInt(value.to_string()),
        }
    }
}

impl From<Vec<Json>> for Json {
    fn from(value: Vec<Json>) -> Self {
        Json::Arr(value)
    }
}

impl From<Vec<String>> for Json {
    fn from(value: Vec<String>) -> Self {
        Json::Arr(value.into_iter().map(Json::Str).collect())
    }
}

impl From<&[String]> for Json {
    fn from(value: &[String]) -> Self {
        Json::Arr(value.iter().map(|item| Json::Str(item.clone())).collect())
    }
}

impl From<Object> for Json {
    fn from(value: Object) -> Self {
        Json::Obj(value)
    }
}

impl<T: Into<Json>> From<Option<T>> for Json {
    fn from(value: Option<T>) -> Self {
        match value {
            Some(inner) => inner.into(),
            None => Json::Null,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParseError {
    TooLarge,
    TooDeep,
    Syntax { offset: usize, what: &'static str },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::TooLarge => f.write_str("JSON text too large"),
            ParseError::TooDeep => f.write_str("JSON nested too deeply"),
            ParseError::Syntax { offset, what } => write!(f, "{what} at byte {offset}"),
        }
    }
}

impl std::error::Error for ParseError {}

pub(crate) fn parse(text: &str) -> Result<Json, ParseError> {
    if text.len() > MAX_TEXT_BYTES {
        return Err(ParseError::TooLarge);
    }
    let mut parser = Parser {
        text,
        bytes: text.as_bytes(),
        pos: 0,
    };
    parser.skip_ws();
    let value = parser.value(0)?;
    parser.skip_ws();
    if parser.pos != parser.bytes.len() {
        return Err(parser.error("trailing characters"));
    }
    Ok(value)
}

struct Parser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn error(&self, what: &'static str) -> ParseError {
        ParseError::Syntax {
            offset: self.pos,
            what,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, ParseError> {
        match self.peek() {
            Some(b'{') => self.object(depth + 1),
            Some(b'[') => self.array(depth + 1),
            Some(b'"') => self.string().map(Json::Str),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'n') => self.literal("null", Json::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.error("unexpected character")),
            None => Err(self.error("unexpected end of text")),
        }
    }

    fn literal(&mut self, word: &'static str, value: Json) -> Result<Json, ParseError> {
        let end = self.pos + word.len();
        if self.bytes.get(self.pos..end) == Some(word.as_bytes()) {
            self.pos = end;
            Ok(value)
        } else {
            Err(self.error("invalid literal"))
        }
    }

    fn array(&mut self, depth: usize) -> Result<Json, ParseError> {
        if depth > MAX_DEPTH {
            return Err(ParseError::TooDeep);
        }
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Json::Arr(items));
        }
        loop {
            self.skip_ws();
            items.push(self.value(depth)?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Json::Arr(items));
                }
                _ => return Err(self.error("expected ',' or ']'")),
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<Json, ParseError> {
        if depth > MAX_DEPTH {
            return Err(ParseError::TooDeep);
        }
        self.pos += 1;
        let mut object = Object::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Json::Obj(object));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(self.error("expected a string as object key"));
            }
            let key = self.string()?;
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(self.error("expected ':'"));
            }
            self.pos += 1;
            self.skip_ws();
            let value = self.value(depth)?;
            match index.get(&key) {
                Some(&position) => object.replace_at(position, value),
                None => {
                    index.insert(key.clone(), object.len());
                    object.push_new(key, value);
                }
            }
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Json::Obj(object));
                }
                _ => return Err(self.error("expected ',' or '}'")),
            }
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.pos;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        self.pos - start
    }

    fn number(&mut self) -> Result<Json, ParseError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => {
                self.digits();
            }
            _ => return Err(self.error("invalid number")),
        }
        let mut is_float = false;
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if self.digits() == 0 {
                return Err(self.error("invalid number"));
            }
            is_float = true;
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if self.digits() == 0 {
                return Err(self.error("invalid number"));
            }
            is_float = true;
        }
        let lexeme = self
            .text
            .get(start..self.pos)
            .ok_or_else(|| self.error("invalid number"))?;
        if !is_float {
            return Ok(match lexeme.parse::<i64>() {
                Ok(number) => Json::Int(number),
                Err(_) => Json::BigInt(lexeme.to_string()),
            });
        }
        match lexeme.parse::<f64>() {
            Ok(number) if number.is_finite() => Ok(Json::Float(number)),
            _ => Err(self.error("number out of range")),
        }
    }

    fn hex4(&mut self) -> Result<u32, ParseError> {
        let digits = self
            .bytes
            .get(self.pos..self.pos + 4)
            .ok_or_else(|| self.error("truncated \\u escape"))?;
        let mut value = 0u32;
        for &digit in digits {
            let nibble = match digit {
                b'0'..=b'9' => digit - b'0',
                b'a'..=b'f' => digit - b'a' + 10,
                b'A'..=b'F' => digit - b'A' + 10,
                _ => return Err(self.error("invalid \\u escape")),
            };
            value = value * 16 + u32::from(nibble);
        }
        self.pos += 4;
        Ok(value)
    }

    fn unicode_escape(&mut self) -> Result<char, ParseError> {
        let first = self.hex4()?;
        if (0xD800..0xDC00).contains(&first) {
            if self.bytes.get(self.pos..self.pos + 2) == Some(b"\\u") {
                let saved = self.pos;
                self.pos += 2;
                let second = self.hex4()?;
                if (0xDC00..0xE000).contains(&second) {
                    let combined = 0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00);
                    return Ok(char::from_u32(combined).unwrap_or('\u{fffd}'));
                }
                self.pos = saved;
            }
            return Ok('\u{fffd}');
        }
        if (0xDC00..0xE000).contains(&first) {
            return Ok('\u{fffd}');
        }
        Ok(char::from_u32(first).unwrap_or('\u{fffd}'))
    }

    fn string(&mut self) -> Result<String, ParseError> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            let start = self.pos;
            while let Some(byte) = self.peek() {
                if byte == b'"' || byte == b'\\' || byte < 0x20 {
                    break;
                }
                self.pos += 1;
            }
            let run = self
                .text
                .get(start..self.pos)
                .ok_or_else(|| self.error("invalid text"))?;
            out.push_str(run);
            match self.peek() {
                None => return Err(self.error("unterminated string")),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.pos += 1;
                    let escape = self
                        .peek()
                        .ok_or_else(|| self.error("unterminated string"))?;
                    self.pos += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.unicode_escape()?),
                        _ => return Err(self.error("invalid escape")),
                    }
                }
                Some(_) => return Err(self.error("control character in string")),
            }
        }
    }
}

pub(crate) fn dumps(value: &Json, ensure_ascii: bool) -> String {
    let mut out = String::new();
    write_value(&mut out, value, ensure_ascii, None, 0);
    out
}

pub(crate) fn dumps_pretty(value: &Json, indent: usize, ensure_ascii: bool) -> String {
    let mut out = String::new();
    write_value(&mut out, value, ensure_ascii, Some(indent), 0);
    out
}

fn newline(out: &mut String, indent: usize, level: usize) {
    out.push('\n');
    for _ in 0..indent.saturating_mul(level) {
        out.push(' ');
    }
}

fn write_value(
    out: &mut String,
    value: &Json,
    ensure_ascii: bool,
    indent: Option<usize>,
    level: usize,
) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(true) => out.push_str("true"),
        Json::Bool(false) => out.push_str("false"),
        Json::Int(number) => {
            let _ = write!(out, "{number}");
        }
        Json::BigInt(text) => out.push_str(text),
        Json::Float(number) => {
            if number.is_finite() {
                out.push_str(&number.to_string());
            } else {
                out.push_str("null");
            }
        }
        Json::Str(text) => write_string(out, text, ensure_ascii),
        Json::Arr(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (position, item) in items.iter().enumerate() {
                if position > 0 {
                    out.push_str(if indent.is_some() { "," } else { ", " });
                }
                if let Some(width) = indent {
                    newline(out, width, level + 1);
                }
                write_value(out, item, ensure_ascii, indent, level + 1);
            }
            if let Some(width) = indent {
                newline(out, width, level);
            }
            out.push(']');
        }
        Json::Obj(object) => {
            if object.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (position, (key, item)) in object.iter().enumerate() {
                if position > 0 {
                    out.push_str(if indent.is_some() { "," } else { ", " });
                }
                if let Some(width) = indent {
                    newline(out, width, level + 1);
                }
                write_string(out, key, ensure_ascii);
                out.push_str(": ");
                write_value(out, item, ensure_ascii, indent, level + 1);
            }
            if let Some(width) = indent {
                newline(out, width, level);
            }
            out.push('}');
        }
    }
}

fn write_string(out: &mut String, text: &str, ensure_ascii: bool) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c if ensure_ascii && u32::from(c) > 0x7e => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{:04x}", unit);
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestResult;

    fn ok(text: &str) -> Result<Json, String> {
        parse(text).map_err(|error| format!("{text:?} should parse: {error}"))
    }

    fn bad(text: &str) {
        assert!(parse(text).is_err(), "{text:?} must be rejected");
    }

    #[test]
    fn parses_scalars() -> TestResult {
        assert_eq!(ok("null")?, Json::Null);
        assert_eq!(ok(" true ")?, Json::Bool(true));
        assert_eq!(ok("false")?, Json::Bool(false));
        assert_eq!(ok("0")?, Json::Int(0));
        assert_eq!(ok("-0")?, Json::Int(0));
        assert_eq!(ok("-12")?, Json::Int(-12));
        assert_eq!(ok("1.5")?, Json::Float(1.5));
        assert_eq!(ok("1e3")?, Json::Float(1000.0));
        assert_eq!(ok("-1.25E-2")?, Json::Float(-0.0125));
        assert_eq!(ok("\"a\"")?, Json::Str("a".into()));
        Ok(())
    }

    #[test]
    fn integers_beyond_i64_keep_their_digits() -> TestResult {
        assert_eq!(
            ok("9223372036854775807")?,
            Json::Int(9_223_372_036_854_775_807)
        );
        assert_eq!(
            ok("9223372036854775808")?,
            Json::BigInt("9223372036854775808".into())
        );
        assert_eq!(
            ok("-123456789012345678901234567890")?,
            Json::BigInt("-123456789012345678901234567890".into())
        );
        Ok(())
    }

    #[test]
    fn rejects_malformed_numbers() {
        for text in [
            "01",
            "+1",
            "1.",
            ".5",
            "-",
            "-a",
            "1e",
            "1e+",
            "0x10",
            "NaN",
            "Infinity",
            "-Infinity",
            "1e999",
            "--1",
            "1.2.3",
            "[01]",
        ] {
            bad(text);
        }
    }

    #[test]
    fn parses_all_string_escapes() -> TestResult {
        assert_eq!(
            ok(r#""\"\\\/\b\f\n\r\t""#)?,
            Json::Str("\"\\/\u{8}\u{c}\n\r\t".into())
        );
        assert_eq!(ok(r#""\u0041\u00e4\u20AC""#)?, Json::Str("Aä€".into()));
        assert_eq!(ok("\"ä€😀\"")?, Json::Str("ä€😀".into()));
        Ok(())
    }

    #[test]
    fn combines_surrogate_pairs() -> TestResult {
        assert_eq!(ok(r#""\ud83d\ude00""#)?, Json::Str("😀".into()));
        assert_eq!(ok(r#""\uD83D\uDE00x""#)?, Json::Str("😀x".into()));
        Ok(())
    }

    #[test]
    fn lone_surrogates_become_the_replacement_character() -> TestResult {
        assert_eq!(ok(r#""\ud800""#)?, Json::Str("\u{fffd}".into()));
        assert_eq!(ok(r#""\udc00""#)?, Json::Str("\u{fffd}".into()));
        assert_eq!(ok(r#""\ud800\u0041""#)?, Json::Str("\u{fffd}A".into()));
        assert_eq!(ok(r#""\ud800x""#)?, Json::Str("\u{fffd}x".into()));
        Ok(())
    }

    #[test]
    fn rejects_malformed_strings() {
        for text in [
            "\"abc",
            "\"a\nb\"",
            "\"a\u{1}b\"",
            r#""\x""#,
            r#""\u12""#,
            r#""\u12G4""#,
            r#""\"#,
            r#""\ud800\u12""#,
        ] {
            bad(text);
        }
    }

    #[test]
    fn parses_containers() -> TestResult {
        assert_eq!(ok("[]")?, Json::Arr(vec![]));
        assert_eq!(ok("{}")?, Json::Obj(Object::new()));
        assert_eq!(
            ok(r#" [1, "a", [true], {"k": null}] "#)?,
            Json::Arr(vec![
                Json::Int(1),
                Json::Str("a".into()),
                Json::Arr(vec![Json::Bool(true)]),
                Json::object(vec![("k", Json::Null)]),
            ])
        );
        Ok(())
    }

    #[test]
    fn object_keys_keep_their_order_and_duplicates_collapse() -> TestResult {
        let value = ok(r#"{"b": 1, "a": 2, "b": 3}"#)?;
        let object = value.as_object().ok_or("object")?;
        let keys: Vec<&str> = object.keys().collect();
        assert_eq!(keys, ["b", "a"]);
        assert_eq!(object.get("b"), Some(&Json::Int(3)));
        Ok(())
    }

    #[test]
    fn rejects_malformed_documents() {
        for text in [
            "",
            "   ",
            "{",
            "[",
            "[1,]",
            "[1 2]",
            "{\"a\":1,}",
            "{\"a\" 1}",
            "{a:1}",
            "{'a':1}",
            "tru",
            "nul",
            "True",
            "1 2",
            "{} x",
            "[] []",
            "\u{feff}1",
            "\u{a0}1",
        ] {
            bad(text);
        }
    }

    #[test]
    fn rejects_nesting_beyond_the_limit_without_overflowing_the_stack() {
        let deep = "[".repeat(MAX_DEPTH + 1) + &"]".repeat(MAX_DEPTH + 1);
        assert_eq!(parse(&deep), Err(ParseError::TooDeep));
        let fine = "[".repeat(MAX_DEPTH) + &"]".repeat(MAX_DEPTH);
        assert!(parse(&fine).is_ok());
        let huge = "[".repeat(1_000_000);
        assert_eq!(parse(&huge), Err(ParseError::TooDeep));
        let objects = "{\"a\":".repeat(10_000);
        assert_eq!(parse(&objects), Err(ParseError::TooDeep));
    }

    #[test]
    fn rejects_texts_beyond_the_size_limit() {
        let text = format!("\"{}\"", "a".repeat(MAX_TEXT_BYTES));
        assert_eq!(parse(&text), Err(ParseError::TooLarge));
    }

    #[test]
    fn dumps_compact_with_spaces() {
        let value = Json::object(vec![
            ("jsonrpc", Json::from("2.0")),
            ("id", Json::Int(1)),
            (
                "result",
                Json::object(vec![
                    ("a", Json::Arr(vec![])),
                    ("b", Json::Obj(Object::new())),
                    (
                        "c",
                        Json::Arr(vec![Json::Int(1), Json::Null, Json::Bool(false)]),
                    ),
                ]),
            ),
        ]);
        assert_eq!(
            dumps(&value, true),
            r#"{"jsonrpc": "2.0", "id": 1, "result": {"a": [], "b": {}, "c": [1, null, false]}}"#
        );
    }

    #[test]
    fn dumps_pretty_with_two_space_indent() {
        let value = Json::object(vec![
            ("a", Json::Arr(vec![])),
            ("b", Json::Obj(Object::new())),
            ("c", Json::Arr(vec![Json::Int(1), Json::Int(2)])),
            ("d", Json::object(vec![("e", Json::from("ü"))])),
        ]);
        let expected = "{\n  \"a\": [],\n  \"b\": {},\n  \"c\": [\n    1,\n    2\n  ],\n  \"d\": {\n    \"e\": \"ü\"\n  }\n}";
        assert_eq!(dumps_pretty(&value, 2, false), expected);
    }

    #[test]
    fn ascii_escaping_uses_unicode_escapes() {
        let text = Json::from("a\"b\\c\n\r\t\u{8}\u{c}\u{1}\u{7f}é€😀");
        assert_eq!(
            dumps(&text, true),
            r#""a\"b\\c\n\r\t\b\f\u0001\u007f\u00e9\u20ac\ud83d\ude00""#
        );
        assert_eq!(
            dumps(&text, false),
            "\"a\\\"b\\\\c\\n\\r\\t\\b\\f\\u0001\u{7f}é€😀\""
        );
    }

    #[test]
    fn floats_and_big_integers_round_trip() -> TestResult {
        assert_eq!(dumps(&Json::Float(1.5), true), "1.5");
        assert_eq!(dumps(&Json::Float(100.0), true), "100");
        assert_eq!(dumps(&Json::Float(1e16), true), "10000000000000000");
        assert_eq!(dumps(&Json::Float(0.00001), true), "0.00001");
        assert_eq!(dumps(&Json::Float(-0.0), true), "-0");
        assert_eq!(
            dumps(&ok("12345678901234567890123")?, true),
            "12345678901234567890123"
        );
        assert_eq!(dumps(&Json::Float(f64::NAN), true), "null");
        Ok(())
    }

    #[test]
    fn insert_replaces_in_place_and_appends() {
        let mut object = Object::new();
        object.insert("a", 1i64);
        object.insert("b", 2i64);
        object.insert("a", 3i64);
        object.insert("c", 4i64);
        let pairs: Vec<(&str, &Json)> = object.iter().collect();
        assert_eq!(
            pairs,
            [
                ("a", &Json::Int(3)),
                ("b", &Json::Int(2)),
                ("c", &Json::Int(4))
            ]
        );
        assert_eq!(object.remove("b"), Some(Json::Int(2)));
        assert_eq!(object.remove("b"), None);
        assert_eq!(object.len(), 2);
    }

    #[test]
    fn as_bool_only_reads_booleans() {
        assert_eq!(Json::Bool(true).as_bool(), Some(true));
        assert_eq!(Json::Bool(false).as_bool(), Some(false));
        assert_eq!(Json::Int(1).as_bool(), None);
        assert_eq!(Json::from("true").as_bool(), None);
        assert_eq!(Json::Null.as_bool(), None);
    }

    #[test]
    fn survives_arbitrary_bytes_without_panicking() {
        let seed = r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"x","arguments":{"a":[1,2.5,"\u00e4\ud83d\ude00"],"b":null}}}"#;
        for end in 0..=seed.len() {
            if let Some(prefix) = seed.get(..end) {
                let _ = parse(prefix);
            }
        }
        for position in 0..seed.len() {
            for replacement in [
                "\"", "\\", "{", "}", "[", "]", ",", ":", "\u{0}", "é", "9", "-", "e",
            ] {
                let mut mutated = String::new();
                for (index, c) in seed.chars().enumerate() {
                    if index == position {
                        mutated.push_str(replacement);
                    } else {
                        mutated.push(c);
                    }
                }
                let _ = parse(&mutated);
            }
        }
    }
}
