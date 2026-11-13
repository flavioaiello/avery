const MAX_HTML: usize = 2 * 1024 * 1024;
const MAX_REFERENCE: usize = 12;
const MAX_NAME: usize = 32;
const RAW: [&str; 3] = ["script", "style", "title"];
const BLOCKS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "center",
    "dd",
    "details",
    "dir",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "li",
    "main",
    "menu",
    "nav",
    "ol",
    "section",
    "summary",
    "table",
    "tbody",
    "tfoot",
    "thead",
    "tr",
    "ul",
];
const LATIN1: &[&str] = &[
    "nbsp", "iexcl", "cent", "pound", "curren", "yen", "brvbar", "sect", "uml", "copy", "ordf",
    "laquo", "not", "shy", "reg", "macr", "deg", "plusmn", "sup2", "sup3", "acute", "micro",
    "para", "middot", "cedil", "sup1", "ordm", "raquo", "frac14", "frac12", "frac34", "iquest",
    "Agrave", "Aacute", "Acirc", "Atilde", "Auml", "Aring", "AElig", "Ccedil", "Egrave", "Eacute",
    "Ecirc", "Euml", "Igrave", "Iacute", "Icirc", "Iuml", "ETH", "Ntilde", "Ograve", "Oacute",
    "Ocirc", "Otilde", "Ouml", "times", "Oslash", "Ugrave", "Uacute", "Ucirc", "Uuml", "Yacute",
    "THORN", "szlig", "agrave", "aacute", "acirc", "atilde", "auml", "aring", "aelig", "ccedil",
    "egrave", "eacute", "ecirc", "euml", "igrave", "iacute", "icirc", "iuml", "eth", "ntilde",
    "ograve", "oacute", "ocirc", "otilde", "ouml", "divide", "oslash", "ugrave", "uacute", "ucirc",
    "uuml", "yacute", "thorn", "yuml",
];
const LATIN1_START: u32 = 160;

#[derive(Default)]
struct Sink {
    text: String,
    pending: bool,
    pre: u32,
}

impl Sink {
    fn trim_spaces(&mut self) {
        while self.text.ends_with(' ') {
            self.text.pop();
        }
    }

    fn text(&mut self, c: char) {
        if self.pre > 0 {
            if c != '\r' {
                self.text.push(c);
            }
        } else if c.is_whitespace() {
            self.pending = true;
        } else {
            self.literal(c);
        }
    }

    fn literal(&mut self, c: char) {
        if self.pending && !self.text.is_empty() && !self.text.ends_with([' ', '\n']) {
            self.text.push(' ');
        }
        self.pending = false;
        self.text.push(if c == '\u{a0}' { ' ' } else { c });
    }

    fn hard_break(&mut self) {
        self.trim_spaces();
        if !self.text.ends_with("\n\n") {
            self.text.push('\n');
        }
        self.pending = false;
    }

    fn soft_break(&mut self) {
        self.trim_spaces();
        if !self.text.is_empty() && !self.text.ends_with('\n') {
            self.text.push('\n');
        }
        self.pending = false;
    }

    fn paragraph(&mut self) {
        self.soft_break();
        if !self.text.is_empty() && !self.text.ends_with("\n\n") {
            self.text.push('\n');
        }
    }

    fn element(&mut self, name: &str, closing: bool) {
        match name {
            "br" => self.hard_break(),
            "p" => self.paragraph(),
            "pre" => {
                self.soft_break();
                self.pre = if closing {
                    self.pre.saturating_sub(1)
                } else {
                    self.pre.saturating_add(1)
                };
            }
            "td" | "th" => self.pending = true,
            other if BLOCKS.contains(&other) => self.soft_break(),
            _ => {}
        }
    }

    fn finish(self) -> String {
        self.text.trim().to_string()
    }
}

fn named(name: &str) -> Option<char> {
    let found = match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "hellip" => '\u{2026}',
        "ndash" => '\u{2013}',
        "mdash" => '\u{2014}',
        "lsquo" => '\u{2018}',
        "rsquo" => '\u{2019}',
        "sbquo" => '\u{201a}',
        "ldquo" => '\u{201c}',
        "rdquo" => '\u{201d}',
        "bdquo" => '\u{201e}',
        "bull" => '\u{2022}',
        "euro" => '\u{20ac}',
        "trade" => '\u{2122}',
        "larr" => '\u{2190}',
        "uarr" => '\u{2191}',
        "rarr" => '\u{2192}',
        "darr" => '\u{2193}',
        "ensp" => '\u{2002}',
        "emsp" => '\u{2003}',
        "thinsp" => '\u{2009}',
        "zwnj" => '\u{200c}',
        "zwj" => '\u{200d}',
        "lrm" => '\u{200e}',
        "rlm" => '\u{200f}',
        "dagger" => '\u{2020}',
        "permil" => '\u{2030}',
        "lsaquo" => '\u{2039}',
        "rsaquo" => '\u{203a}',
        "minus" => '\u{2212}',
        "OElig" => '\u{152}',
        "oelig" => '\u{153}',
        "Scaron" => '\u{160}',
        "scaron" => '\u{161}',
        "Yuml" => '\u{178}',
        _ => {
            let index = LATIN1.iter().position(|known| *known == name)?;
            return char::from_u32(LATIN1_START + u32::try_from(index).ok()?);
        }
    };
    Some(found)
}

fn numeric(number: &str) -> Option<char> {
    let (digits, radix) = match number.strip_prefix(['x', 'X']) {
        Some(hex) => (hex, 16),
        None => (number, 10),
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    let code = u32::from_str_radix(digits, radix).ok()?;
    Some(
        char::from_u32(code)
            .filter(|c| *c != '\0')
            .unwrap_or('\u{fffd}'),
    )
}

fn decode(name: &str) -> Option<char> {
    match name.strip_prefix('#') {
        Some(number) => numeric(number),
        None => named(name),
    }
}

fn reference(rest: &str, sink: &mut Sink) -> usize {
    let tail = rest.get(1..).unwrap_or_default();
    let close = tail
        .char_indices()
        .take(MAX_REFERENCE + 1)
        .find(|(_, c)| *c == ';')
        .map(|(index, _)| index);
    let decoded = close.and_then(|end| tail.get(..end)).and_then(decode);
    match (close, decoded) {
        (Some(end), Some(c)) => {
            sink.literal(c);
            end + 2
        }
        _ => {
            sink.literal('&');
            1
        }
    }
}

fn to_close(rest: &str) -> usize {
    rest.find('>').map_or(rest.len(), |index| index + 1)
}

fn comment(rest: &str) -> usize {
    let body = rest.get(4..).unwrap_or_default();
    if body.starts_with('>') {
        return 5;
    }
    if body.starts_with("->") {
        return 6;
    }
    body.find("-->").map_or(rest.len(), |index| index + 7)
}

fn tag_length(rest: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    let mut value = false;
    for (index, c) in rest.char_indices() {
        match quote {
            Some(open) => {
                if c == open {
                    quote = None;
                }
            }
            None => match c {
                '>' => return Some(index + 1),
                '"' | '\'' if value => {
                    quote = Some(c);
                    value = false;
                }
                '=' => value = true,
                other if other.is_whitespace() => {}
                _ => value = false,
            },
        }
    }
    None
}

fn raw_length(tail: &str, name: &str) -> usize {
    for (index, _) in tail.match_indices("</") {
        let start = index + 2;
        let named = tail
            .get(start..start + name.len())
            .is_some_and(|word| word.eq_ignore_ascii_case(name));
        let ended = tail
            .get(start + name.len()..)
            .and_then(|after| after.chars().next())
            .is_some_and(|c| c == '>' || c == '/' || c.is_whitespace());
        if named && ended {
            return tail
                .get(index..)
                .and_then(|from| from.find('>'))
                .map_or(tail.len(), |close| index + close + 1);
        }
    }
    tail.len()
}

fn tag(rest: &str, skip: usize, closing: bool, sink: &mut Sink) -> usize {
    let name: String = rest
        .get(skip..)
        .unwrap_or_default()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '-'))
        .take(MAX_NAME)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let Some(length) = tag_length(rest) else {
        return rest.len();
    };
    if !closing && RAW.contains(&name.as_str()) {
        let tail = rest.get(length..).unwrap_or_default();
        return length + raw_length(tail, &name);
    }
    sink.element(&name, closing);
    length
}

fn markup(rest: &str, sink: &mut Sink) -> usize {
    let after = rest.get(1..).unwrap_or_default();
    if after.starts_with("!--") {
        return comment(rest);
    }
    let mut chars = after.chars();
    match (chars.next(), chars.next()) {
        (Some(first), _) if first.is_ascii_alphabetic() => tag(rest, 1, false, sink),
        (Some('/'), Some(second)) if second.is_ascii_alphabetic() => tag(rest, 2, true, sink),
        (Some('!' | '?' | '/'), _) => to_close(rest),
        _ => {
            sink.literal('<');
            1
        }
    }
}

fn cut(html: &str) -> &str {
    let mut end = html.len().min(MAX_HTML);
    while end > 0 && !html.is_char_boundary(end) {
        end -= 1;
    }
    html.get(..end).unwrap_or_default()
}

pub(crate) fn to_text(html: &str) -> String {
    let source = cut(html);
    let mut sink = Sink::default();
    let mut position = 0;
    while let Some(rest) = source.get(position..) {
        let Some(next) = rest.chars().next() else {
            break;
        };
        position += match next {
            '<' => markup(rest, &mut sink),
            '&' => reference(rest, &mut sink),
            other => {
                sink.text(other);
                other.len_utf8()
            }
        };
    }
    sink.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn tags_are_stripped_and_text_is_kept() {
        assert_eq!(to_text("Hello <b>big</b> <i>world</i>"), "Hello big world");
        assert_eq!(to_text(""), "");
        assert_eq!(to_text("plain"), "plain");
        assert_eq!(to_text("<b></b>"), "");
        assert_eq!(to_text("<A HREF=\"http://x.example\">Link</A>"), "Link");
        assert_eq!(to_text("<o:p></o:p>Text<o:p>&nbsp;</o:p>"), "Text");
    }

    #[test]
    fn breaks_follow_br_p_div_tr_and_li() {
        assert_eq!(to_text("a<br>b<br/>c<BR />d"), "a\nb\nc\nd");
        assert_eq!(to_text("a<br><br>b"), "a\n\nb");
        assert_eq!(to_text("a<br><br><br><br>b"), "a\n\nb");
        assert_eq!(to_text("<p>one</p><p>two</p>"), "one\n\ntwo");
        assert_eq!(to_text("<div>one</div><div>two</div>"), "one\ntwo");
        assert_eq!(
            to_text("<div><div>one</div></div><div>two</div>"),
            "one\ntwo"
        );
        assert_eq!(to_text("<ul><li>a</li><li>b</li></ul>"), "a\nb");
        assert_eq!(
            to_text("<table><tr><td>a</td><td>b</td></tr><tr><td>c</td><th>d</th></tr></table>"),
            "a b\nc d"
        );
        assert_eq!(to_text("<h1>Title</h1>Body"), "Title\nBody");
        assert_eq!(to_text("x<hr>y"), "x\ny");
        assert_eq!(to_text("<br>lead"), "lead");
        assert_eq!(to_text("tail<br><br>"), "tail");
    }

    #[test]
    fn whitespace_collapses_like_in_a_browser() {
        assert_eq!(to_text("a \r\n\t b"), "a b");
        assert_eq!(
            to_text("<div>\r\n  Hello\r\n  <b>there</b>  \r\n</div>"),
            "Hello there"
        );
        assert_eq!(to_text("a<span> </span>b"), "a b");
        assert_eq!(to_text("a  <br>  b"), "a\nb");
        assert_eq!(to_text("  \n  "), "");
    }

    #[test]
    fn preformatted_text_keeps_its_whitespace() {
        assert_eq!(to_text("<pre>a  b\r\n  c</pre>after"), "a  b\n  c\nafter");
        assert_eq!(to_text("x<pre>  y</pre>z"), "x\n  y\nz");
        assert_eq!(to_text("</pre></pre>a  b"), "a b");
    }

    #[test]
    fn script_style_and_title_content_is_dropped() {
        assert_eq!(
            to_text("a<script>var x = '<b>';</script>b<style>p {color: red}</style>c"),
            "abc"
        );
        assert_eq!(
            to_text("<HEAD><TITLE>Hidden &amp; secret</TITLE></HEAD><body>Shown</body>"),
            "Shown"
        );
        assert_eq!(
            to_text("a<SCRIPT type=\"text/javascript\">if (a < b) {}</Script >b"),
            "ab"
        );
        assert_eq!(to_text("a<style\n>x</style\n>b"), "ab");
        assert_eq!(to_text("a<script>x</scriptx>y</script>b"), "ab");
        assert_eq!(to_text("a<script>x</script/>b"), "ab");
        assert_eq!(to_text("a<script>never closed <b>tail"), "a");
        assert_eq!(to_text("a<script/>hidden</script>b"), "ab");
        assert_eq!(to_text("a</script>b"), "ab");
    }

    #[test]
    fn comments_doctypes_and_instructions_are_dropped() {
        assert_eq!(to_text("a<!-- hidden <b>text</b> -->b"), "ab");
        assert_eq!(to_text("a<!---->b<!-->c<!--->d"), "abcd");
        assert_eq!(
            to_text("a<!--[if gte mso 9]><xml><o:x>1</o:x></xml><![endif]-->b"),
            "ab"
        );
        assert_eq!(to_text("a<!-- never closed b"), "a");
        assert_eq!(to_text("<!DOCTYPE html><html>x</html>"), "x");
        assert_eq!(to_text("<?xml version=\"1.0\"?>x"), "x");
        assert_eq!(to_text("a<![if !supportLists]>b<![endif]>c"), "abc");
        assert_eq!(to_text("a</>b</ 3>c"), "abc");
    }

    #[test]
    fn attribute_values_may_contain_angle_brackets_and_quotes() {
        assert_eq!(to_text("<a title=\"a>b\">x</a>"), "x");
        assert_eq!(to_text("<a title='a>b'>x</a>"), "x");
        assert_eq!(to_text("<a title = \"a>b\" href=x>y</a>"), "y");
        assert_eq!(to_text("<img alt=\"it's\" src=a>z"), "z");
        assert_eq!(to_text("<a b\"c>x</a>"), "x");
        assert_eq!(to_text("<a title=\"never closed>x</a>y"), "");
    }

    #[test]
    fn stray_angle_brackets_stay_text() {
        assert_eq!(to_text("5 < 6 and 7 > 3"), "5 < 6 and 7 > 3");
        assert_eq!(to_text("I <3 you"), "I <3 you");
        assert_eq!(to_text("a <"), "a <");
        assert_eq!(to_text("a<= b"), "a<= b");
        assert_eq!(to_text("<"), "<");
        assert_eq!(to_text("x<b"), "x");
        assert_eq!(to_text("x<b y=\"z\""), "x");
    }

    #[test]
    fn entities_are_decoded() {
        assert_eq!(
            to_text("&lt;b&gt; &amp; &quot;q&quot; &apos;a&apos; &#39;b&#39;"),
            "<b> & \"q\" 'a' 'b'"
        );
        assert_eq!(to_text("&#65;&#x42;&#X43;&#x1F600;"), "ABC\u{1f600}");
        assert_eq!(
            to_text("&auml;&ouml;&uuml;&szlig;&Auml;&euro;&hellip;&mdash;&copy;&yuml;"),
            "\u{e4}\u{f6}\u{fc}\u{df}\u{c4}\u{20ac}\u{2026}\u{2014}\u{a9}\u{ff}"
        );
        assert_eq!(to_text("a&nbsp;b&nbsp;&nbsp;c"), "a b  c");
        assert_eq!(
            to_text("&lt;script&gt;alert(1)&lt;/script&gt;"),
            "<script>alert(1)</script>"
        );
        assert_eq!(to_text("&amp;lt;"), "&lt;");
    }

    #[test]
    fn malformed_entities_stay_as_they_are() {
        for same in [
            "&",
            "a & b",
            "&;",
            "&unknown;",
            "&amp",
            "&#;",
            "&#x;",
            "&#xZZ;",
            "&#12a;",
            "&notanentityatall;",
            "&#99999999999;",
            "AT&T;",
            "&&",
        ] {
            assert_eq!(to_text(same), same, "{same:?}");
        }
        assert_eq!(to_text("&&amp;"), "&&");
        assert_eq!(
            to_text("&#0;&#xD800;&#x110000;"),
            "\u{fffd}\u{fffd}\u{fffd}"
        );
        assert_eq!(to_text("&#128512;"), "\u{1f600}");
    }

    #[test]
    fn the_latin1_table_covers_every_code_point_from_160_to_255() {
        assert_eq!(LATIN1.len(), 96);
        assert_eq!(named("nbsp"), Some('\u{a0}'));
        assert_eq!(named("iquest"), Some('\u{bf}'));
        assert_eq!(named("Agrave"), Some('\u{c0}'));
        assert_eq!(named("times"), Some('\u{d7}'));
        assert_eq!(named("divide"), Some('\u{f7}'));
        assert_eq!(named("yuml"), Some('\u{ff}'));
        assert_eq!(named("Auml"), Some('\u{c4}'));
        assert_eq!(named("auml"), Some('\u{e4}'));
        assert_eq!(named("AUML"), None);
        for (index, name) in LATIN1.iter().enumerate() {
            let expected = u32::try_from(index)
                .ok()
                .and_then(|offset| char::from_u32(160 + offset));
            assert_eq!(named(name), expected, "{name}");
        }
    }

    #[test]
    fn hidden_characters_from_entities_survive_for_the_caller_to_remove() {
        assert_eq!(to_text("a&#8203;b&shy;c"), "a\u{200b}b\u{ad}c");
    }

    #[test]
    fn multibyte_text_around_markup_is_safe() {
        assert_eq!(
            to_text("\u{e4}<b>\u{20ac}</b>\u{1f600}<br>\u{4e2d}\u{6587}"),
            "\u{e4}\u{20ac}\u{1f600}\n\u{4e2d}\u{6587}"
        );
        assert_eq!(to_text("\u{1f600}&amp;\u{1f600}"), "\u{1f600}&\u{1f600}");
        assert_eq!(to_text("<\u{e4}>x"), "<\u{e4}>x");
        assert_eq!(to_text("<a \u{e4}=\"\u{20ac}\">x</a>"), "x");
    }

    #[test]
    fn a_typical_outlook_message_becomes_readable_text() {
        let html = concat!(
            "<html>\r\n<head>\r\n<meta http-equiv=\"Content-Type\" content=\"text/html; charset=utf-8\">\r\n",
            "<style type=\"text/css\" style=\"display:none;\"> P {margin-top:0;margin-bottom:0;} </style>\r\n",
            "</head>\r\n<body dir=\"ltr\">\r\n",
            "<div style=\"font-family: Calibri; font-size: 12pt;\">Hi Bob,</div>\r\n",
            "<div style=\"font-family: Calibri; font-size: 12pt;\"><br></div>\r\n",
            "<div>the invoice is attached.&nbsp;Total: 84,20&nbsp;&euro;</div>\r\n",
            "<div><br></div>\r\n",
            "<div id=\"Signature\"><div>Best,<br>Alice</div></div>\r\n",
            "</body>\r\n</html>\r\n"
        );
        assert_eq!(
            to_text(html),
            "Hi Bob,\n\nthe invoice is attached. Total: 84,20 \u{20ac}\n\nBest,\nAlice"
        );
    }

    #[test]
    fn input_beyond_the_limit_is_cut_at_a_character_boundary() {
        let mut html = "a".repeat(MAX_HTML - 1);
        html.push('\u{20ac}');
        html.push_str("<b>tail</b>");
        let text = to_text(&html);
        assert_eq!(text.chars().count(), MAX_HTML - 1);
        assert!(!text.contains("tail"));
        let exact = "b".repeat(MAX_HTML);
        assert_eq!(to_text(&exact).len(), MAX_HTML);
        assert_eq!(cut("abc"), "abc");
        assert_eq!(cut(""), "");
    }

    #[test]
    fn hostile_input_is_processed_in_linear_time() {
        let started = Instant::now();
        let unterminated = "<a ".repeat(MAX_HTML / 3);
        assert_eq!(to_text(&unterminated), "");
        let quotes = "<a b=\"".repeat(MAX_HTML / 6);
        assert_eq!(to_text(&quotes), "");
        let comments = "<!--".repeat(MAX_HTML / 4);
        assert_eq!(to_text(&comments), "");
        let nested = "<div>".repeat(MAX_HTML / 5) + "x";
        assert_eq!(to_text(&nested), "x");
        let scripts = "<script>".repeat(MAX_HTML / 8);
        assert_eq!(to_text(&scripts), "");
        let closers = "<script></ ".repeat(MAX_HTML / 11);
        assert_eq!(to_text(&closers), "");
        let entities = "&amp".repeat(MAX_HTML / 4);
        assert_eq!(to_text(&entities).len(), MAX_HTML);
        let ampersands = "&".repeat(MAX_HTML);
        assert_eq!(to_text(&ampersands).len(), MAX_HTML);
        assert!(started.elapsed() < Duration::from_secs(20));
    }

    #[test]
    fn truncated_and_mutated_documents_never_panic_and_never_grow() {
        let seed = concat!(
            "<!DOCTYPE html><html><head><title>T</title><style>p{}</style></head>",
            "<body class=\"a>b\"><!-- c --><p>Hi &amp; &#x42;&#999999999; &nbsp;<br/>",
            "<script>if (a<b) {}</script><a href='x'>\u{e4}\u{20ac}\u{1f600}</a>",
            "<pre> x </pre><table><tr><td>1</td><th>2</th></tr></table></body></html>"
        );
        for end in 0..=seed.len() {
            if let Some(prefix) = seed.get(..end) {
                assert!(to_text(prefix).len() <= prefix.len());
            }
        }
        for (position, _) in seed.char_indices() {
            for replacement in [
                "<", ">", "&", ";", "\"", "'", "=", "/", "!", "-", "\u{0}", "\u{e4}", "#",
            ] {
                let mut mutated = String::new();
                for (index, c) in seed.char_indices() {
                    if index == position {
                        mutated.push_str(replacement);
                    } else {
                        mutated.push(c);
                    }
                }
                assert!(to_text(&mutated).len() <= mutated.len() + 4);
            }
        }
    }
}
