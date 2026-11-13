use crate::text::is_hidden;

const MAX_INPUT: usize = 2 * 1024 * 1024;
const MAX_NAME: usize = 32;
const MAX_REFERENCE: usize = 12;
const RAW: [&str; 3] = ["script", "style", "title"];
const LATIN1_START: u32 = 160;
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
const NAMED: &[(&str, char)] = &[
    ("amp", '&'),
    ("lt", '<'),
    ("gt", '>'),
    ("quot", '"'),
    ("apos", '\''),
    ("ensp", '\u{2002}'),
    ("emsp", '\u{2003}'),
    ("thinsp", '\u{2009}'),
    ("zwnj", '\u{200c}'),
    ("zwj", '\u{200d}'),
    ("lrm", '\u{200e}'),
    ("rlm", '\u{200f}'),
    ("ndash", '\u{2013}'),
    ("mdash", '\u{2014}'),
    ("lsquo", '\u{2018}'),
    ("rsquo", '\u{2019}'),
    ("sbquo", '\u{201a}'),
    ("ldquo", '\u{201c}'),
    ("rdquo", '\u{201d}'),
    ("bdquo", '\u{201e}'),
    ("dagger", '\u{2020}'),
    ("bull", '\u{2022}'),
    ("hellip", '\u{2026}'),
    ("permil", '\u{2030}'),
    ("lsaquo", '\u{2039}'),
    ("rsaquo", '\u{203a}'),
    ("euro", '\u{20ac}'),
    ("trade", '\u{2122}'),
    ("larr", '\u{2190}'),
    ("uarr", '\u{2191}'),
    ("rarr", '\u{2192}'),
    ("darr", '\u{2193}'),
    ("minus", '\u{2212}'),
    ("OElig", '\u{152}'),
    ("oelig", '\u{153}'),
    ("Scaron", '\u{160}'),
    ("scaron", '\u{161}'),
    ("Yuml", '\u{178}'),
];

enum Layout {
    Collapse,
    Lines,
}

struct Sink {
    text: String,
    layout: Layout,
    pre: usize,
    pending: bool,
}

impl Sink {
    fn new(capacity: usize, layout: Layout) -> Sink {
        Sink {
            text: String::with_capacity(capacity),
            layout,
            pre: 0,
            pending: false,
        }
    }

    fn keeps_spacing(&self) -> bool {
        self.pre > 0 || matches!(self.layout, Layout::Lines)
    }

    fn newlines(&self) -> usize {
        self.text
            .bytes()
            .rev()
            .take(2)
            .take_while(|byte| *byte == b'\n')
            .count()
    }

    fn settle(&mut self) -> bool {
        self.pending = false;
        while self.text.ends_with([' ', '\t']) {
            self.text.pop();
        }
        !self.text.is_empty()
    }

    fn breaks(&mut self, wanted: usize) {
        if self.settle() {
            for _ in self.newlines()..wanted {
                self.text.push('\n');
            }
        }
    }

    fn line(&mut self) {
        if self.settle() && self.newlines() < 2 {
            self.text.push('\n');
        }
    }

    fn visible(&mut self, c: char) {
        if self.pending && !self.text.is_empty() && !self.text.ends_with([' ', '\t', '\n']) {
            self.text.push(' ');
        }
        self.pending = false;
        self.text.push(c);
    }

    fn text(&mut self, c: char) {
        if c.is_ascii_graphic() {
            self.visible(c);
        } else if c == '\n' {
            if self.keeps_spacing() {
                self.line();
            } else {
                self.pending = true;
            }
        } else if c.is_whitespace() {
            if !self.keeps_spacing() {
                self.pending = true;
            } else if c == '\t' {
                self.text.push('\t');
            } else {
                self.text.push(' ');
            }
        } else if !is_hidden(c) {
            self.visible(c);
        }
    }

    fn element(&mut self, name: &str, closing: bool) {
        match name {
            "br" => self.line(),
            "p" => self.breaks(2),
            "pre" => {
                self.breaks(1);
                self.pre = if closing {
                    self.pre.saturating_sub(1)
                } else {
                    self.pre.saturating_add(1)
                };
            }
            "td" | "th" => self.pending = true,
            other if is_block(other) => self.breaks(1),
            _ => {}
        }
    }

    fn finish(self) -> String {
        self.text.trim().to_string()
    }
}

fn is_block(name: &str) -> bool {
    matches!(
        name,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "caption"
            | "center"
            | "dd"
            | "details"
            | "dir"
            | "div"
            | "dl"
            | "dt"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hr"
            | "li"
            | "main"
            | "menu"
            | "nav"
            | "ol"
            | "section"
            | "summary"
            | "table"
            | "tbody"
            | "tfoot"
            | "thead"
            | "tr"
            | "ul"
    )
}

fn named(name: &str) -> Option<char> {
    if let Some((_, found)) = NAMED.iter().find(|(known, _)| *known == name) {
        return Some(*found);
    }
    let index = LATIN1.iter().position(|known| *known == name)?;
    char::from_u32(LATIN1_START + u32::try_from(index).ok()?)
}

fn numeric(number: &str) -> Option<char> {
    let (digits, radix) = match number.strip_prefix(['x', 'X']) {
        Some(hex) => (hex, 16),
        None => (number, 10),
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    char::from_u32(u32::from_str_radix(digits, radix).ok()?)
}

fn decode(rest: &str) -> Option<(char, usize)> {
    let tail = rest.get(1..)?;
    let end = tail
        .bytes()
        .take(MAX_REFERENCE + 1)
        .position(|byte| byte == b';')?;
    let name = tail.get(..end)?;
    let decoded = match name.strip_prefix('#') {
        Some(number) => numeric(number),
        None => named(name),
    }?;
    Some((decoded, end + 2))
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
    let mut quote: Option<u8> = None;
    let mut value = false;
    for (index, byte) in rest.bytes().enumerate() {
        match quote {
            Some(open) => {
                if byte == open {
                    quote = None;
                }
            }
            None => match byte {
                b'>' => return Some(index + 1),
                b'"' | b'\'' if value => {
                    quote = Some(byte);
                    value = false;
                }
                b'=' => value = true,
                other if other.is_ascii_whitespace() => {}
                _ => value = false,
            },
        }
    }
    None
}

fn raw_length(tail: &str, name: &str) -> Option<usize> {
    for (index, _) in tail.match_indices("</") {
        let start = index + 2;
        let named = tail
            .get(start..start + name.len())
            .is_some_and(|word| word.eq_ignore_ascii_case(name));
        let ended = tail
            .get(start + name.len()..)
            .and_then(|after| after.chars().next())
            .is_some_and(|c| c == '>' || c == '/' || c.is_ascii_whitespace());
        if named && ended {
            let closer = tail.get(index..).unwrap_or_default();
            return Some(index + tag_length(closer).unwrap_or(closer.len()));
        }
    }
    None
}

fn cut(html: &str) -> &str {
    let mut end = html.len().min(MAX_INPUT);
    while end > 0 && !html.is_char_boundary(end) {
        end -= 1;
    }
    html.get(..end).unwrap_or_default()
}

struct Converter<'a> {
    source: &'a str,
    position: usize,
    sink: Sink,
    no_head_end: bool,
}

impl<'a> Converter<'a> {
    fn rest(&self) -> &'a str {
        self.source.get(self.position..).unwrap_or_default()
    }

    fn skip_raw(&mut self, name: &str) {
        let tail = self.rest();
        self.position += raw_length(tail, name).unwrap_or(tail.len());
    }

    fn skip_head(&mut self) {
        if self.no_head_end {
            return;
        }
        match raw_length(self.rest(), "head") {
            Some(length) => self.position += length,
            None => self.no_head_end = true,
        }
    }

    fn tag(&mut self, skip: usize, closing: bool) {
        let rest = self.rest();
        let name: String = rest
            .get(skip..)
            .unwrap_or_default()
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '-'))
            .take(MAX_NAME)
            .map(|c| c.to_ascii_lowercase())
            .collect();
        let Some(length) = tag_length(rest) else {
            self.position += rest.len();
            return;
        };
        self.position += length;
        match (closing, name.as_str()) {
            (false, raw) if RAW.contains(&raw) => self.skip_raw(raw),
            (false, "head") => self.skip_head(),
            (_, other) => self.sink.element(other, closing),
        }
    }

    fn markup(&mut self) {
        let rest = self.rest();
        let after = rest.get(1..).unwrap_or_default();
        if after.starts_with("!--") {
            self.position += comment(rest);
            return;
        }
        let mut chars = after.chars();
        match (chars.next(), chars.next()) {
            (Some(first), _) if first.is_ascii_alphabetic() => self.tag(1, false),
            (Some('/'), Some(second)) if second.is_ascii_alphabetic() => self.tag(2, true),
            (Some('!' | '?' | '/'), _) => self.position += to_close(rest),
            _ => {
                self.position += 1;
                self.sink.text('<');
            }
        }
    }

    fn reference(&mut self) {
        match decode(self.rest()) {
            Some((decoded, length)) => {
                self.position += length;
                self.sink.text(decoded);
            }
            None => {
                self.position += 1;
                self.sink.text('&');
            }
        }
    }

    fn carriage_return(&mut self) {
        self.position += 1;
        if !self.rest().starts_with('\n') {
            self.sink.text('\n');
        }
    }

    fn run(mut self) -> String {
        while let Some(next) = self.rest().chars().next() {
            match next {
                '<' => self.markup(),
                '&' => self.reference(),
                '\r' => self.carriage_return(),
                other => {
                    self.position += other.len_utf8();
                    self.sink.text(other);
                }
            }
        }
        self.sink.finish()
    }
}

fn convert(html: &str, layout: Layout) -> String {
    let source = cut(html);
    Converter {
        source,
        position: 0,
        sink: Sink::new(source.len(), layout),
        no_head_end: false,
    }
    .run()
}

pub(crate) fn to_text(html: &str) -> String {
    convert(html, Layout::Collapse)
}

pub(crate) fn to_notes(html: &str) -> String {
    convert(html, Layout::Lines)
}

pub(crate) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestResult;
    use std::time::{Duration, Instant};

    fn well_formed(out: &str) -> bool {
        out == out.trim()
            && !out.contains("\n\n\n")
            && !out.contains(" \n")
            && !out.contains("\t\n")
            && !out.contains('\r')
            && !out.chars().any(is_hidden)
            && !out
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t')
    }

    fn both(html: &str) -> [String; 2] {
        [to_text(html), to_notes(html)]
    }

    const HOSTILE: usize = MAX_INPUT / 8;

    fn filled(piece: &str, bytes: usize) -> String {
        piece.repeat(bytes.checked_div(piece.len()).unwrap_or_default())
    }

    #[test]
    fn breaks_follow_br_p_div_tr_and_li() {
        assert_eq!(to_text("<p>Hello</p><p>World</p>"), "Hello\n\nWorld");
        assert_eq!(
            to_text("Hello<br>World<BR/>again<br />!"),
            "Hello\nWorld\nagain\n!"
        );
        assert_eq!(to_text("a<br>b<br/>c<BR />d"), "a\nb\nc\nd");
        assert_eq!(to_text("a<br>b<br><br>c<br/>d"), "a\nb\n\nc\nd");
        assert_eq!(to_text("a<br><br>b"), "a\n\nb");
        assert_eq!(to_text("<div>a</div><div>b</div>"), "a\nb");
        assert_eq!(to_text("vor<div>mitte</div>nach"), "vor\nmitte\nnach");
        assert_eq!(
            to_text("<div><div>one</div></div><div>two</div>"),
            "one\ntwo"
        );
        assert_eq!(to_text("<ul><li>one</li><li>two</li></ul>"), "one\ntwo");
        assert_eq!(
            to_text("<table><tr><td>a</td><td>b</td></tr><tr><td>c</td><th>d</th></tr></table>"),
            "a b\nc d"
        );
        assert_eq!(to_text("<h1>Title</h1>text<hr>more"), "Title\ntext\nmore");
        assert_eq!(to_text("<h1>Title</h1>Body"), "Title\nBody");
        assert_eq!(to_text("x<hr>y"), "x\ny");
        assert_eq!(to_text("a<br><br><br><br>b"), "a\n\nb");
        assert_eq!(to_text("x<br><br><br><br>y"), "x\n\ny");
        assert_eq!(to_text("<p></p><p></p>x<p></p>"), "x");
        assert_eq!(to_text("<blockquote>q</blockquote>after"), "q\nafter");
        assert_eq!(to_text("<br>lead"), "lead");
        assert_eq!(to_text("tail<br><br>"), "tail");
        assert_eq!(to_text("<div><br><br><br><br>x</div>"), "x");
        assert_eq!(to_text("<br><br>"), "");
    }

    #[test]
    fn paragraphs_are_set_off_by_blank_lines_and_headings_by_one_break() {
        assert_eq!(to_text("<p>one</p><p>two</p>"), "one\n\ntwo");
        assert_eq!(to_text("<p>a</p><p>b</p>"), "a\n\nb");
        assert_eq!(to_text("a<p>b</p>c"), "a\n\nb\n\nc");
        assert_eq!(to_text("<div>a</div><p>b</p><div>c</div>"), "a\n\nb\n\nc");
        assert_eq!(to_text("<h1>Head</h1>text"), "Head\ntext");
        assert_eq!(to_text("<h2>a</h2><h3>b</h3>"), "a\nb");
        assert_eq!(to_text("<div>a</div><br>b"), "a\n\nb");
    }

    #[test]
    fn list_items_are_plain_lines_and_table_cells_are_separated_by_a_space() {
        assert_eq!(
            to_text("<ul><li>Brot</li><li>Milch</li></ul>Ende"),
            "Brot\nMilch\nEnde"
        );
        assert_eq!(to_text("<ol><li>one<li>two</ol>"), "one\ntwo");
        assert_eq!(to_text("<ul><li>x</li><li>y</li></ul>"), "x\ny");
        assert_eq!(
            to_text("<table><tr><td>a</td><td>b</td></tr><tr><td>c</td></tr></table>"),
            "a b\nc"
        );
        assert_eq!(
            to_text("<table><tr><td>a</td><td>b</td></tr></table>"),
            "a b"
        );
        assert_eq!(
            to_text("<table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table>"),
            "a b\nc d"
        );
        assert_eq!(
            to_text("<table><tr><th>h1</th><th>h2</th></tr><tr><td>1</td><td></td><td>3</td></tr></table>"),
            "h1 h2\n1 3"
        );
        assert_eq!(to_text("a<td>b"), "a b");
        assert_eq!(to_text("<dl><dt>k</dt><dd>v</dd></dl>"), "k\nv");
        assert_eq!(to_text("<ul><li>a</li><li>b</li></ul>"), "a\nb");
        assert_eq!(to_text("<div>one</div><div>two</div>"), "one\ntwo");
    }

    #[test]
    fn tags_are_stripped_and_their_text_is_kept() {
        assert_eq!(to_text("Hello <b>big</b> <i>world</i>"), "Hello big world");
        assert_eq!(to_text(""), "");
        assert_eq!(to_text("plain"), "plain");
        assert_eq!(to_text("<b></b>"), "");
        assert_eq!(to_text("<A HREF=\"http://x.example\">Link</A>"), "Link");
        assert_eq!(to_text("<o:p></o:p>Text<o:p>&nbsp;</o:p>"), "Text");
        assert_eq!(to_text("<o:p></o:p>Outlook<o:p>&nbsp;</o:p>"), "Outlook");
        assert_eq!(
            to_text("<html><head><meta charset=\"utf-8\"></head><body><b>bold</b> and <a href=\"https://x.example/?a=1&b=2\">a link</a></body></html>"),
            "bold and a link"
        );
        assert_eq!(to_text("<img src=\"x.png\" alt=\"pic\">text"), "text");
        assert_eq!(to_text("<img src=x alt=y>Bild"), "Bild");
        assert_eq!(to_text("<A HREF='x'>Up</A><SPAN>Per</SPAN>"), "UpPer");
        assert_eq!(to_text("<span style='color: red'>rot</span>"), "rot");
        assert_eq!(to_text("<DIV>A</DIV><Div>B</Div>"), "A\nB");
        assert_eq!(to_text("<span>a</span><span>b</span>"), "ab");
        assert_eq!(
            to_text("<verylongtagnamethatmakesnosense>x</verylongtagnamethatmakesnosense>"),
            "x"
        );
        assert_eq!(
            to_text(
                r#"Siehe <a href="https://example.org/x?a=1&amp;b=2" target="_blank">hier</a> und <b>fett</b> <i>kursiv</i>"#
            ),
            "Siehe hier und fett kursiv"
        );
    }

    #[test]
    fn tag_names_are_bounded_and_case_insensitive() {
        let long = format!("a<p{}>b", "q".repeat(100));
        assert_eq!(to_text(&long), "ab");
        let huge = format!("a<{}>b", "z".repeat(500_000));
        assert_eq!(to_text(&huge), "ab");
        assert_eq!(to_text("a<P>b</P>c"), "a\n\nb\n\nc");
        assert_eq!(to_text("a<Br>b"), "a\nb");
        assert_eq!(to_text("a<x-custom>b</x-custom>c"), "abc");
        assert_eq!(to_text("a<h1:foo>b</h1:foo>c"), "abc");
    }

    #[test]
    fn markup_text_never_reaches_the_output() {
        assert_eq!(
            to_text("<a href=\"SECRET\" onclick=\"evil()\" data-x='<b>'>x</a>"),
            "x"
        );
        assert_eq!(to_text("<img alt=\"hidden words\" src=x>after"), "after");
        assert_eq!(to_text("<p class=a\nid=b\n>x</p>"), "x");
        assert_eq!(to_text("<input value=\"v\"><br/>t"), "t");
    }

    #[test]
    fn script_style_and_title_content_is_dropped() {
        assert_eq!(
            to_text("<head><title>The title</title><style>p { color: red; } a > b {}</style></head><body>Visible<script>var x = '<p>hidden</p>'; if (a < b) { }</script> end</body>"),
            "Visible end"
        );
        assert_eq!(
            to_text("a<SCRIPT type=\"text/javascript\">x</SCRIPT>b"),
            "ab"
        );
        assert_eq!(to_text("a<style>never closed"), "a");
        assert_eq!(to_text("a<script>x</script >b"), "ab");
        assert_eq!(to_text("a<script>x</script"), "a");
        assert_eq!(to_text("a<script>x</SCRIPT>b<script>y</script>c"), "abc");
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
        assert_eq!(
            to_text("a<script>alert('x'); if (a < b) {}</script>b<style>p { color: red }</style>c"),
            "abc"
        );
        assert_eq!(to_text("a<SCRIPT type=x>evil()</ScRiPt>b"), "ab");
        assert_eq!(to_text("a<script>never closed and <b>more"), "a");
        assert_eq!(to_text("a<script>never closed</b>"), "a");
        assert_eq!(to_text("a<style>x</style >b"), "ab");
        assert_eq!(to_text("a<script>x</script \n>b"), "ab");
        assert_eq!(to_text("a<script>x</script y=\">\">b"), "ab");
        assert_eq!(to_text("a<title>t</title>b<title>u</title>c"), "abc");
    }

    #[test]
    fn head_content_is_dropped_only_when_the_head_is_closed() {
        assert_eq!(
            to_text("<html><head><title>T</title><meta charset=\"utf-8\"></head><body>Body</body></html>"),
            "Body"
        );
        assert_eq!(to_text("<head>text in head</head>body"), "body");
        assert_eq!(to_text("<head><body>kept</body>"), "kept");
        assert_eq!(to_text("<head>unclosed text"), "unclosed text");
        assert_eq!(to_text("<head>a</head>b<head>c"), "bc");
        assert_eq!(to_text("<header>visible</header>"), "visible");
        assert_eq!(to_text("<heady>x</heady>"), "x");
        assert_eq!(to_text("<HEAD>x</HEAD >y"), "y");
        assert_eq!(
            to_text("<head><style>p{}</style></head><body><p>x</p></body>"),
            "x"
        );
    }

    #[test]
    fn comments_declarations_and_instructions_are_dropped() {
        assert_eq!(to_text("a<!-- hidden <b>text</b> -->b"), "ab");
        assert_eq!(to_text("<!DOCTYPE html><html>x</html>"), "x");
        assert_eq!(
            to_text("a<!--[if mso]><table><tr><td>old</td></tr></table><![endif]-->b"),
            "ab"
        );
        assert_eq!(to_text("a<!-- never closed <b>x</b>"), "a");
        assert_eq!(to_text("a<?xml version=\"1.0\"?>b"), "ab");
        assert_eq!(to_text("a<![CDATA[x]]>b"), "ab");
        assert_eq!(to_text("a<!-->b"), "ab");
        assert_eq!(to_text("a<!---->b<!-->c<!--->d"), "abcd");
        assert_eq!(
            to_text("a<!--[if gte mso 9]><xml><o:x>1</o:x></xml><![endif]-->b"),
            "ab"
        );
        assert_eq!(to_text("a<!-- never closed b"), "a");
        assert_eq!(to_text("<?xml version=\"1.0\"?>x"), "x");
        assert_eq!(to_text("a<![if !supportLists]>b<![endif]>c"), "abc");
        assert_eq!(to_text("a</>b</ 3>c"), "abc");
        assert_eq!(to_text("<!DOCTYPE html><html><body>x</body></html>"), "x");
        assert_eq!(to_text("a<!-x>b"), "ab");
        assert_eq!(to_text("a<!-- one --><!-- two -->b"), "ab");
        assert_eq!(to_text("a<!-- <!-- nested --> shown -->b"), "a shown -->b");
    }

    #[test]
    fn entities_are_decoded_once() {
        assert_eq!(
            to_text("Tom &amp; Jerry &lt;3 &gt; &quot;q&quot; &apos;a&apos;"),
            "Tom & Jerry <3 > \"q\" 'a'"
        );
        assert_eq!(to_text("&amp;lt; &amp;amp;"), "&lt; &amp;");
        assert_eq!(to_text("&amp;lt;"), "&lt;");
        assert_eq!(
            to_text("&copy; &reg; &trade; &hellip; &mdash; &ndash;"),
            "\u{a9} \u{ae} \u{2122} \u{2026} \u{2014} \u{2013}"
        );
        assert_eq!(
            to_text("&auml;&ouml;&uuml;&Auml;&Ouml;&Uuml;&szlig;"),
            "\u{e4}\u{f6}\u{fc}\u{c4}\u{d6}\u{dc}\u{df}"
        );
        assert_eq!(
            to_text("&euro;5 &#8364;5 &#x20AC;5 &#X20ac;5"),
            "\u{20ac}5 \u{20ac}5 \u{20ac}5 \u{20ac}5"
        );
        assert_eq!(
            to_text("&lsquo;x&rsquo; &ldquo;y&rdquo;"),
            "\u{2018}x\u{2019} \u{201c}y\u{201d}"
        );
        assert_eq!(
            to_text("&lt;b&gt; &amp; &quot;q&quot; &apos;a&apos; &#39;b&#39;"),
            "<b> & \"q\" 'a' 'b'"
        );
        assert_eq!(to_text("&#65;&#x42;&#X43;&#x1F600;"), "ABC\u{1f600}");
        assert_eq!(
            to_text("&auml;&ouml;&uuml;&szlig;&Auml;&euro;&hellip;&mdash;&copy;&yuml;"),
            "\u{e4}\u{f6}\u{fc}\u{df}\u{c4}\u{20ac}\u{2026}\u{2014}\u{a9}\u{ff}"
        );
        assert_eq!(
            to_text("&lt;script&gt;alert(1)&lt;/script&gt;"),
            "<script>alert(1)</script>"
        );
        assert_eq!(
            to_text("&#65;&#x42;&#X43;&#252;&#x1F600;"),
            "ABC\u{fc}\u{1f600}"
        );
        assert_eq!(
            to_text("&auml;&ouml;&uuml;&Auml;&szlig;&euro;&ndash;&hellip;"),
            "\u{e4}\u{f6}\u{fc}\u{c4}\u{df}\u{20ac}\u{2013}\u{2026}"
        );
        assert_eq!(
            to_text("&#65;&#x42;&#X43; &#8364; &euro;"),
            "ABC \u{20ac} \u{20ac}"
        );
        assert_eq!(to_text("&uuml;&Ouml;&szlig;"), "\u{fc}\u{d6}\u{df}");
        assert_eq!(
            to_text("Q&amp;A &lt;b&gt; &quot;x&quot; &apos;y&apos; a&nbsp;b"),
            "Q&A <b> \"x\" 'y' a b"
        );
        assert_eq!(to_text("&&&&amp;"), "&&&&");
        assert_eq!(to_text("&#00000000065;"), "A");
    }

    #[test]
    fn the_named_entities_of_every_old_table_are_known() {
        assert_eq!(
            to_text("&bull;&middot;&laquo;&raquo;&lsaquo;&rsaquo;&sbquo;&bdquo;"),
            "\u{2022}\u{b7}\u{ab}\u{bb}\u{2039}\u{203a}\u{201a}\u{201e}"
        );
        assert_eq!(
            to_text("&larr;&uarr;&rarr;&darr;&minus;&dagger;&permil;"),
            "\u{2190}\u{2191}\u{2192}\u{2193}\u{2212}\u{2020}\u{2030}"
        );
        assert_eq!(
            to_text("&plusmn;&times;&divide;&frac12;&frac14;&frac34;&sect;&deg;&para;&micro;"),
            "\u{b1}\u{d7}\u{f7}\u{bd}\u{bc}\u{be}\u{a7}\u{b0}\u{b6}\u{b5}"
        );
        assert_eq!(
            to_text("&iexcl;&iquest;&cent;&pound;&yen;&euro;"),
            "\u{a1}\u{bf}\u{a2}\u{a3}\u{a5}\u{20ac}"
        );
        assert_eq!(
            to_text("&OElig;&oelig;&Scaron;&scaron;&Yuml;"),
            "\u{152}\u{153}\u{160}\u{161}\u{178}"
        );
        assert_eq!(
            to_text("&eacute;&egrave;&agrave;&aacute;&acirc;&ccedil;"),
            "\u{e9}\u{e8}\u{e0}\u{e1}\u{e2}\u{e7}"
        );
        assert_eq!(to_text("a&ensp;&emsp;&thinsp;b"), "a b");
    }

    #[test]
    fn unknown_or_invalid_references_stay_literal() {
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
            "AT&T and R&D",
            "&averyveryverylongentityname;",
            "&#",
            "&+41; &#+65;",
            "&#+65; &#-1; &#x+41;",
            "&AUML;",
            "&unknown; &amp &#; &#x; &#xZZ; &#12a; & ;",
            "AT&T &unknown; & done &",
            "&unknown; &amp &; &#; &#x; &#xZZ; &#99999999999;",
            "&#000000000065;",
        ] {
            assert_eq!(to_text(same), same, "{same:?}");
            assert_eq!(to_notes(same), same, "{same:?}");
        }
        assert_eq!(to_text("&&amp;"), "&&");
        assert_eq!(to_text("&#128512;"), "\u{1f600}");
    }

    #[test]
    fn numeric_references_outside_unicode_stay_literal() {
        for same in [
            "&#1114112;",
            "&#x110000;",
            "&#55296;",
            "&#xD800;",
            "&#xDBFF;",
            "&#xDC00;",
            "&#xDFFF;",
            "&#4294967296;",
            "&#xFFFFFFFF;",
            "&#x100000000;",
            "&#xFFFFFFFFF;",
        ] {
            assert_eq!(to_text(same), same, "{same:?}");
            assert_eq!(to_notes(same), same, "{same:?}");
        }
        assert_eq!(to_text("&#0;&#xD800;&#x110000;"), "&#xD800;&#x110000;");
        assert_eq!(
            to_text("&#0; &#xD800; &#; &#x; &#99999999999;"),
            "&#xD800; &#; &#x; &#99999999999;"
        );
        assert_eq!(to_text("&#x10FFFF;"), "\u{10ffff}");
        assert_eq!(to_text("&#1114111;"), "\u{10ffff}");
        assert_eq!(to_text("&#xFFFD;"), "\u{fffd}");
        assert_eq!(to_text("&#x1F600;&#128512;"), "\u{1f600}\u{1f600}");
    }

    #[test]
    fn whitespace_is_collapsed_like_a_browser_does() {
        assert_eq!(to_text("  a \n\t b   c  "), "a b c");
        assert_eq!(to_text("<p>  a  </p>   <p>  b  </p>"), "a\n\nb");
        assert_eq!(to_text("a\u{a0}b"), "a b");
        assert_eq!(to_text("a <b>b</b> c"), "a b c");
        assert_eq!(to_text("a<b> b </b>c"), "a b c");
        assert_eq!(to_text("line one\r\nline two"), "line one line two");
        assert_eq!(to_text("a \r\n\t b"), "a b");
        assert_eq!(
            to_text("<div>\r\n  Hello\r\n  <b>there</b>  \r\n</div>"),
            "Hello there"
        );
        assert_eq!(to_text("a<span> </span>b"), "a b");
        assert_eq!(to_text("a  <br>  b"), "a\nb");
        assert_eq!(to_text("  \n  "), "");
        assert_eq!(to_text("  \n <b>hello</b>   \t world \n "), "hello world");
        assert_eq!(to_text("a\r\n\r\n   b"), "a b");
        assert_eq!(to_text("<div>  a  </div>\n<div>  b  </div>"), "a\nb");
        assert_eq!(to_text("   "), "");
        assert_eq!(to_text("a\u{2003}\u{3000}\u{2028}\u{85}\u{b}\u{c}b"), "a b");
        assert_eq!(to_text("a\rb"), "a b");
        assert_eq!(to_text("a&nbsp;&nbsp;b&nbsp;"), "a b");
        assert_eq!(to_text("a&nbsp;b&nbsp;&nbsp;c"), "a b c");
        assert_eq!(to_text("&nbsp;&nbsp;lead"), "lead");
    }

    #[test]
    fn preformatted_text_keeps_its_lines_and_spacing() {
        assert_eq!(
            to_text("<pre>a\n  b\r\n\nc</pre>after"),
            "a\n  b\n\nc\nafter"
        );
        assert_eq!(to_text("x<pre>\tq &amp; r</pre>"), "x\n\tq & r");
        assert_eq!(to_text("</pre></pre>a"), "a");
        assert_eq!(to_text("<pre>a<pre>b</pre>c</pre>d"), "a\nb\nc\nd");
        assert_eq!(to_text("<pre>a  b\r\n  c</pre>after"), "a  b\n  c\nafter");
        assert_eq!(to_text("x<pre>  y</pre>z"), "x\n  y\nz");
        assert_eq!(to_text("</pre></pre>a  b"), "a b");
        assert_eq!(to_text("<pre>a\r\rb</pre>"), "a\n\nb");
        assert_eq!(to_text("<pre>a\n\n\n\n\nb</pre>"), "a\n\nb");
        assert_eq!(to_text("<pre>a   \nb</pre>"), "a\nb");
        assert_eq!(to_text("<pre>a&nbsp;&nbsp;b</pre>"), "a  b");
        assert_eq!(to_text("<pre>a<br>b</pre>"), "a\nb");
        assert_eq!(to_text("<pre>a</pre>  b  c"), "a\nb c");
    }

    #[test]
    fn a_lone_less_than_sign_is_text() {
        assert_eq!(
            to_text("a < b and 3<4 and x <- y"),
            "a < b and 3<4 and x <- y"
        );
        assert_eq!(to_text("<3 love"), "<3 love");
        assert_eq!(to_text("a<"), "a<");
        assert_eq!(to_text("<"), "<");
        assert_eq!(to_text("a <= b"), "a <= b");
        assert_eq!(to_text("5 < 6 and 7 > 3"), "5 < 6 and 7 > 3");
        assert_eq!(to_text("I <3 you"), "I <3 you");
        assert_eq!(to_text("a <"), "a <");
        assert_eq!(to_text("a<= b"), "a<= b");
        assert_eq!(to_text("a < b and c > d"), "a < b and c > d");
        assert_eq!(to_text("1<2"), "1<2");
        assert_eq!(to_text("x <3 y"), "x <3 y");
        assert_eq!(to_text("a<3"), "a<3");
        assert_eq!(to_text("<\u{e4}>x"), "<\u{e4}>x");
        assert_eq!(to_text("a > b"), "a > b");
    }

    #[test]
    fn an_open_tag_is_markup_even_in_prose() {
        assert_eq!(to_text("a<b and c>d"), "ad");
        assert_eq!(to_text("a<b<c>d"), "ad");
        assert_eq!(to_text("x<b"), "x");
        assert_eq!(to_text("x<b y=\"z\""), "x");
        assert_eq!(to_text("tail <b"), "tail");
        assert_eq!(to_text("</ x>y"), "y");
        assert_eq!(to_text("a <b and c"), "a");
        assert_eq!(to_text("a<b<c"), "a");
    }

    #[test]
    fn quoted_attribute_values_may_contain_the_closing_bracket() {
        assert_eq!(to_text("<a title=\"a>b\">x</a>"), "x");
        assert_eq!(to_text("<a title='a>b'>x</a>"), "x");
        assert_eq!(to_text("<p don't>x</p>"), "x");
        assert_eq!(to_text("<a title = \"a>b\" href=x>y</a>"), "y");
        assert_eq!(to_text("<img alt=\"it's\" src=a>z"), "z");
        assert_eq!(to_text("<a b\"c>x</a>"), "x");
        assert_eq!(to_text("<a title=\"x<y\">t</a>"), "t");
        assert_eq!(to_text("<a \u{e4}=\"\u{20ac}\">x</a>"), "x");
        assert_eq!(to_text("<a href=x\"y>z</a>"), "z");
        assert_eq!(to_text("<a title=\"never closed>x</a>y"), "");
    }

    #[test]
    fn unterminated_markup_drops_the_rest_and_is_never_emitted() {
        for (html, expected) in [
            ("a <b unterminated", "a"),
            ("a <a href=\"x>text", "a"),
            ("a<!-- never closed <b>x</b>", "a"),
            ("a<!DOCTYPE never closed", "a"),
            ("a<?php never closed", "a"),
            ("a</div never closed", "a"),
            ("a<script>never closed <b>x", "a"),
            ("a<style>never closed", "a"),
            ("a<title>never closed", "a"),
            ("a<script>x</script", "a"),
            ("a<script>x</script ", "a"),
            ("a</", "a"),
            ("a<!", "a"),
            ("a<?", "a"),
            ("a<!-", "a"),
            ("a<!--", "a"),
            ("a<b c=\"d", "a"),
            ("a<b c='d", "a"),
            ("a<b c=", "a"),
            ("a<br", "a"),
        ] {
            assert_eq!(to_text(html), expected, "{html:?}");
            assert_eq!(to_notes(html), expected, "{html:?}");
        }
        assert_eq!(to_text(&"<a ".repeat(300_000)), "");
        let huge_tag = format!("x<a {}>y", "q".repeat(1_500_000));
        assert_eq!(to_text(&huge_tag), "xy");
        let unclosed = format!("x<a {}", "q".repeat(1_500_000));
        assert_eq!(to_text(&unclosed), "x");
    }

    #[test]
    fn multibyte_text_around_markup_is_kept_intact() {
        assert_eq!(
            to_text("Gr\u{fc}\u{df}e <b>\u{20ac}</b>\u{1f600}<br>\u{e4}"),
            "Gr\u{fc}\u{df}e \u{20ac}\u{1f600}\n\u{e4}"
        );
        assert_eq!(
            to_text("\u{4e2d}\u{6587}&amp;\u{65e5}\u{672c}"),
            "\u{4e2d}\u{6587}&\u{65e5}\u{672c}"
        );
        assert_eq!(to_text("<p title=\"\u{e4}\">\u{e4}</p>"), "\u{e4}");
        assert_eq!(
            to_text("\u{e4}<b>\u{20ac}</b>\u{1f600}<br>\u{4e2d}\u{6587}"),
            "\u{e4}\u{20ac}\u{1f600}\n\u{4e2d}\u{6587}"
        );
        assert_eq!(to_text("\u{1f600}&amp;\u{1f600}"), "\u{1f600}&\u{1f600}");
        assert_eq!(to_text("\u{1f600}<\u{1f600}>"), "\u{1f600}<\u{1f600}>");
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
    fn every_named_entity_decodes_to_a_distinct_valid_character() {
        let mut seen = Vec::new();
        for (name, decoded) in NAMED {
            assert_eq!(named(name), Some(*decoded), "{name}");
            assert!(!seen.contains(decoded), "{name}");
            seen.push(*decoded);
        }
        for name in LATIN1 {
            let decoded = named(name);
            assert!(decoded.is_some_and(|c| !seen.contains(&c)), "{name}");
            seen.extend(decoded);
        }
        assert_eq!(seen.len(), NAMED.len() + LATIN1.len());
    }

    #[test]
    fn a_realistic_newsletter_reads_as_plain_text() {
        let html = concat!(
            "<!DOCTYPE html><html><head><title>News</title><style>.x{color:red}</style></head>",
            "<body><table width=\"100%\"><tr><td><h1>Hallo Anna,</h1></td></tr>",
            "<tr><td><p>Deine Rechnung &uuml;ber <b>12,50&nbsp;&euro;</b> ist da.</p>",
            "<p><a href=\"https://pay.example/x?id=1&amp;t=2\">Jetzt bezahlen</a></p></td></tr>",
            "<tr><td>Gr&uuml;&szlig;e<br>Dein Team</td></tr></table>",
            "<script>track()</script></body></html>"
        );
        assert_eq!(
            to_text(html),
            "Hallo Anna,\n\nDeine Rechnung \u{fc}ber 12,50 \u{20ac} ist da.\n\nJetzt bezahlen\n\nGr\u{fc}\u{df}e\nDein Team"
        );
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
    fn an_outlook_event_body_becomes_plain_lines() {
        let html = concat!(
            "<html><head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=utf-8\">",
            "<style type=\"text/css\" style=\"display:none\">P {margin-top:0;margin-bottom:0;}</style></head>",
            "<body dir=\"ltr\"><div style=\"font-family: Calibri, Arial; font-size: 12pt; color: rgb(0, 0, 0);\">",
            "Bring the <b>blue</b> folder.<br>Room &amp; coffee at <a href=\"https://example.invalid/x?a=1&b=2\">the desk</a>.",
            "</div><div><br></div><div>Thanks!</div></body></html>"
        );
        assert_eq!(
            to_text(html),
            "Bring the blue folder.\nRoom & coffee at the desk.\n\nThanks!"
        );
    }

    #[test]
    fn a_gmail_reply_with_a_quote_becomes_readable_text() {
        let html = concat!(
            "<div dir=\"ltr\">Hi Anna,<div><br></div><div>Thanks for the update.&nbsp;See you at 10:00.</div>",
            "<div><br></div><div>Bob</div></div><br><div class=\"gmail_quote\">",
            "<div dir=\"ltr\" class=\"gmail_attr\">On Mon, 4 May 2026 at 09:15, Anna &lt;anna@example.org&gt; wrote:<br></div>",
            "<blockquote class=\"gmail_quote\" style=\"margin:0px 0px 0px 0.8ex;border-left:1px solid rgb(204,204,204);padding-left:1ex\">",
            "<div dir=\"ltr\">Quoted text</div></blockquote></div>"
        );
        assert_eq!(
            to_text(html),
            "Hi Anna,\n\nThanks for the update. See you at 10:00.\n\nBob\n\nOn Mon, 4 May 2026 at 09:15, Anna <anna@example.org> wrote:\nQuoted text"
        );
    }

    #[test]
    fn a_google_calendar_description_keeps_its_lines() {
        let html = concat!(
            "<p>Bitte <b>Kuchen</b> mitbringen.</p><ul><li>Teller</li><li>Besteck</li></ul>",
            "Danke &amp; Gruss\n\nJoin: <a href=\"https://meet.example/abc\">meet.example/abc</a>\nPIN: 1234"
        );
        assert_eq!(
            to_notes(html),
            "Bitte Kuchen mitbringen.\n\nTeller\nBesteck\nDanke & Gruss\n\nJoin: meet.example/abc\nPIN: 1234"
        );
    }

    #[test]
    fn input_beyond_the_cap_is_cut_at_a_character_boundary() {
        let mut html = "a".repeat(MAX_INPUT - 1);
        html.push('\u{20ac}');
        html.push_str("<b>tail</b>");
        for text in both(&html) {
            assert_eq!(text.chars().count(), MAX_INPUT - 1);
            assert!(!text.contains("tail"));
        }
        let exact = "b".repeat(MAX_INPUT);
        assert_eq!(to_text(&exact).len(), MAX_INPUT);
        assert_eq!(to_text(&format!("{exact}tail")).len(), MAX_INPUT);
        assert_eq!(to_notes(&format!("{exact}<script>")).len(), MAX_INPUT);
        assert_eq!(cut("abc"), "abc");
        assert_eq!(cut(""), "");
        assert_eq!(cut(&"\u{1f600}".repeat(MAX_INPUT)).len(), MAX_INPUT);
    }

    #[test]
    fn deep_nesting_is_handled_without_recursion() {
        let deep = format!("{}x{}", "<div>".repeat(150_000), "</div>".repeat(150_000));
        assert_eq!(to_text(&deep), "x");
        let spans = format!("{}x{}", "<span>".repeat(150_000), "</span>".repeat(150_000));
        assert_eq!(to_text(&spans), "x");
        let tables = format!(
            "{}x{}",
            "<table><tr><td>".repeat(60_000),
            "</td></tr></table>".repeat(60_000)
        );
        assert_eq!(to_text(&tables), "x");
        let pre = format!("{}x{}", "<pre>".repeat(100_000), "</pre>".repeat(100_000));
        assert_eq!(to_text(&pre), "x");
        let opened = "<pre>".repeat(MAX_INPUT / 5 - 10) + "a \n b";
        assert_eq!(to_text(&opened), "a\n b");
        let closers = "</div></pre></p></td>".repeat(90_000);
        assert_eq!(to_text(&closers), "");
        let items = "<ul><li>a".repeat(100_000);
        assert_eq!(to_text(&items).lines().count(), 100_000);
    }

    #[test]
    fn very_long_runs_stay_linear_and_small() {
        let letters = "a".repeat(MAX_INPUT);
        assert_eq!(to_text(&letters).len(), MAX_INPUT);
        assert_eq!(to_text(&" ".repeat(MAX_INPUT)), "");
        assert_eq!(to_text(&"\n".repeat(MAX_INPUT)), "");
        assert_eq!(to_notes(&"\n".repeat(MAX_INPUT)), "");
        assert_eq!(to_notes(&" ".repeat(MAX_INPUT)), "");
        assert_eq!(to_text(&"<br>".repeat(MAX_INPUT / 4)), "");
        assert_eq!(to_text(&"a ".repeat(MAX_INPUT / 2)).len(), MAX_INPUT - 1);
        assert_eq!(to_text(&"a\n".repeat(MAX_INPUT / 2)).len(), MAX_INPUT - 1);
        assert_eq!(
            to_notes(&"a\n\n\n\n".repeat(MAX_INPUT / 5))
                .matches("\n\n")
                .count(),
            MAX_INPUT / 5 - 1
        );
        assert_eq!(to_text(&"&nbsp;".repeat(MAX_INPUT / 6)), "");
        assert_eq!(to_text(&"x<br>".repeat(100_000)).lines().count(), 100_000);
        assert_eq!(to_text(&"x<p>".repeat(100_000)).lines().count(), 199_999);
        let words = to_text(&"<p>word</p>".repeat(100_000));
        assert_eq!(
            words.lines().filter(|line| *line == "word").count(),
            100_000
        );
        assert_eq!(words.lines().count(), 199_999);
        assert_eq!(to_text(&"&amp;".repeat(100_000)).chars().count(), 100_000);
    }

    #[test]
    fn notes_keep_the_line_breaks_of_the_source() {
        assert_eq!(
            to_notes("Bitte Kuchen mitbringen."),
            "Bitte Kuchen mitbringen."
        );
        assert_eq!(
            to_notes("Zeile 1\nZeile 2\r\nZeile 3"),
            "Zeile 1\nZeile 2\nZeile 3"
        );
        assert_eq!(
            to_text("Zeile 1\nZeile 2\r\nZeile 3"),
            "Zeile 1 Zeile 2 Zeile 3"
        );
        assert_eq!(to_notes("  \n oben\n\n\n\n unten \n  "), "oben\n\n unten");
        assert_eq!(to_notes(""), "");
        assert_eq!(to_notes("   \n\t "), "");
        assert_eq!(to_notes("a\rb"), "a\nb");
        assert_eq!(to_notes("a\r\rb"), "a\n\nb");
        assert_eq!(to_notes("a  b\tc"), "a  b\tc");
        assert_eq!(to_notes("a\u{a0}\u{a0}b"), "a  b");
        assert_eq!(to_notes("a   \nb"), "a\nb");
        assert_eq!(to_notes("a \t \n \t b"), "a\n \t b");
    }

    #[test]
    fn notes_take_their_breaks_from_the_markup_and_from_the_source() {
        assert_eq!(to_notes("a<br>b<br/>c<BR />d"), "a\nb\nc\nd");
        assert_eq!(to_notes("a<br><br>b"), "a\n\nb");
        assert_eq!(to_notes("<p>eins</p><p>zwei</p>"), "eins\n\nzwei");
        assert_eq!(to_notes("<div>eins</div><div>zwei</div>"), "eins\nzwei");
        assert_eq!(to_notes("vor<div>mitte</div>nach"), "vor\nmitte\nnach");
        assert_eq!(
            to_notes("<ul><li>Brot</li><li>Milch</li></ul>Ende"),
            "Brot\nMilch\nEnde"
        );
        assert_eq!(to_notes("<h1>Titel</h1>Text"), "Titel\nText");
        assert_eq!(
            to_notes("<table><tr><td>a</td><td>b</td></tr><tr><td>c</td></tr></table>"),
            "a b\nc"
        );
        assert_eq!(to_notes("<pre>a  b\n  c</pre>"), "a  b\n  c");
        assert_eq!(to_notes("<p>a</p>\n<p>b</p>"), "a\n\nb");
        assert_eq!(to_notes("a<br>\nb"), "a\n\nb");
        assert_eq!(
            to_notes(
                "Siehe <a href=\"https://example.org/x?a=1&amp;b=2\">hier</a>  und  <b>fett</b>"
            ),
            "Siehe hier  und  fett"
        );
        assert_eq!(
            to_text(
                "Siehe <a href=\"https://example.org/x?a=1&amp;b=2\">hier</a>  und  <b>fett</b>"
            ),
            "Siehe hier und fett"
        );
    }

    #[test]
    fn both_layouts_drop_the_same_markup_and_decode_the_same_references() {
        for (html, expected) in [
            ("a<script>x</script>b<style>p{}</style>c", "abc"),
            ("a<!-- c -->b<?php x ?>c<!DOCTYPE html>d", "abcd"),
            ("<head><title>t</title></head>body", "body"),
            (
                "&lt;b&gt; &amp; &quot;q&quot; &#65;&#x42;",
                "<b> & \"q\" AB",
            ),
            ("a&#13;b&#7;c", "a bc"),
            ("&#0; &#xD800; &#1114112;", "&#xD800; &#1114112;"),
            ("a<o:p></o:p>b", "ab"),
            ("a<IMG SRC='x' ALT=\"y\">b", "ab"),
        ] {
            for out in both(html) {
                assert_eq!(out, expected, "{html:?}");
            }
        }
    }

    #[test]
    fn control_characters_never_reach_the_output() {
        for html in [
            "a\u{0}b",
            "a&#0;b",
            "a&#x0;b",
            "a\u{1}\u{2}\u{3}\u{4}\u{5}\u{6}\u{7}\u{8}b",
            "a\u{e}\u{f}\u{10}\u{1b}\u{1f}b",
            "a\u{7f}b",
            "a\u{80}\u{81}\u{84}\u{86}\u{8d}\u{9f}b",
            "a&#1;&#8;&#14;&#31;&#127;&#128;&#159;b",
            "a&#x7F;&#x80;&#x9F;&#x1B;b",
            "\u{0}a\u{0}b\u{0}",
            "<span\u{0}>a</span\u{0}>b",
            "<a title=\"\u{0}\u{1}\">a</a>b",
            "a<!-- \u{0} -->b",
        ] {
            for out in both(html) {
                assert_eq!(out, "ab", "{html:?}");
            }
        }
        assert_eq!(to_text("a\u{b}b\u{c}c\u{85}d"), "a b c d");
        assert_eq!(to_text("a&#9;b&#10;c&#13;d"), "a b c d");
        assert_eq!(to_notes("a&#9;b&#10;c"), "a\tb\nc");
        assert_eq!(to_text("<pre>a&#9;b&#0;c</pre>"), "a\tbc");
        assert_eq!(to_text("<p\u{0}>a</p\u{0}>b"), "a\n\nb");
    }

    #[test]
    fn invisible_and_bidirectional_characters_are_dropped() {
        for html in [
            "a\u{200b}b",
            "a\u{200c}\u{200d}\u{200e}\u{200f}b",
            "a\u{202a}\u{202b}\u{202c}\u{202d}\u{202e}b",
            "a\u{2060}\u{2066}\u{2067}\u{2068}\u{2069}b",
            "\u{feff}a\u{feff}b\u{feff}",
            "a\u{ad}b",
            "a&#8203;&#x200E;&shy;&zwnj;&zwj;&lrm;&rlm;b",
            "a&#x202E;&#8238;b",
            "a\u{e0001}\u{e0041}b",
            "a\u{fe0f}\u{180e}b",
            "<a title=\"\u{202e}\">a</a>\u{200b}b",
        ] {
            for out in both(html) {
                assert_eq!(out, "ab", "{html:?}");
            }
        }
        assert_eq!(to_text("\u{202e}evil.exe"), "evil.exe");
        assert_eq!(to_text("a \u{200b} b"), "a b");
        assert_eq!(
            to_text("caf\u{e9} \u{1f600} a\u{200b}b \u{202e}x"),
            "caf\u{e9} \u{1f600} ab x"
        );
        assert_eq!(to_text("a&#8203;b&shy;c"), "abc");
    }

    #[test]
    fn cdata_sections_and_processing_instructions_are_skipped() {
        assert_eq!(to_text("a<![CDATA[x]]>b"), "ab");
        assert_eq!(to_text("a<![CDATA[never closed"), "a");
        assert_eq!(to_text("a<![CDATA["), "a");
        assert_eq!(to_text("a<![CDATA[x]]"), "a");
        assert_eq!(to_text("a<?xml version=\"1.0\"?>b<?php echo 1 ?>c"), "abc");
        assert_eq!(to_text("a<?never closed"), "a");
        assert_eq!(to_text("<?xml?><![CDATA[x]]><!-- y --><!DOCTYPE z>w"), "w");
    }

    #[test]
    fn entity_bombs_are_decoded_once() {
        assert_eq!(to_text("&amp;amp;amp;amp;"), "&amp;amp;amp;");
        assert_eq!(to_text("&amp;lt;script&amp;gt;"), "&lt;script&gt;");
        assert_eq!(to_text("&amp;#65; &amp;#x41;"), "&#65; &#x41;");
        let amp = "&amp;".repeat(MAX_INPUT / 5);
        assert_eq!(to_text(&amp), "&".repeat(MAX_INPUT / 5));
        assert_eq!(to_notes(&amp), "&".repeat(MAX_INPUT / 5));
        let lt = "&lt;b&gt;".repeat(MAX_INPUT / 9);
        assert_eq!(to_text(&lt), "<b>".repeat(MAX_INPUT / 9));
        let numeric = "&#x10FFFF;".repeat(MAX_INPUT / 10);
        assert_eq!(to_text(&numeric).chars().count(), MAX_INPUT / 10);
        let unknown = "&unknown;".repeat(MAX_INPUT / 9);
        assert_eq!(
            to_text(&unknown),
            unknown.get(..MAX_INPUT / 9 * 9).unwrap_or_default()
        );
        let digits = format!("&#{}65;", "0".repeat(1_500_000));
        assert_eq!(to_text(&digits), digits);
        let hex = format!("&#x{}41;", "0".repeat(1_500_000));
        assert_eq!(to_text(&hex), hex);
        let name = format!("&{};", "a".repeat(1_500_000));
        assert_eq!(to_text(&name), name);
    }

    #[test]
    fn very_long_attribute_values_are_skipped_without_buffering() {
        let value = ">".repeat(1_500_000);
        let html = format!("a<a title=\"{value}\">b</a>c");
        for out in both(&html) {
            assert_eq!(out, "abc");
        }
        let single = format!("a<a title='{value}'>b</a>c");
        assert_eq!(to_text(&single), "abc");
        let bare = format!("a<a title={}>b</a>c", "q".repeat(1_500_000));
        assert_eq!(to_text(&bare), "abc");
        let many = format!("a<a {}>b", "x=y ".repeat(300_000));
        assert_eq!(to_text(&many), "ab");
        let script = format!("a<script>{}</script>b", "x".repeat(1_500_000));
        assert_eq!(to_text(&script), "ab");
        let style = format!("a<style>{}</style>b", "<".repeat(1_500_000));
        assert_eq!(to_text(&style), "ab");
        let swallowed = format!("a<script>{}", "x".repeat(MAX_INPUT));
        assert_eq!(to_text(&swallowed), "a");
        let at_the_cap = format!("{}<script>x</script>b", "a".repeat(MAX_INPUT - 12));
        assert_eq!(to_text(&at_the_cap), "a".repeat(MAX_INPUT - 12));
    }

    #[test]
    fn markup_cut_by_the_input_cap_is_dropped() {
        let tag = format!("{}<b>after", "a".repeat(MAX_INPUT - 2));
        assert_eq!(to_text(&tag), "a".repeat(MAX_INPUT - 2));
        let entity = format!("{}&amp;after", "a".repeat(MAX_INPUT - 3));
        assert_eq!(
            to_text(&entity),
            format!("{}&am", "a".repeat(MAX_INPUT - 3))
        );
        let comment = format!("{}<!-- x -->after", "a".repeat(MAX_INPUT - 5));
        assert_eq!(to_text(&comment), "a".repeat(MAX_INPUT - 5));
        let whole = format!("{}<br>after", "a".repeat(MAX_INPUT - 4));
        assert_eq!(to_text(&whole), "a".repeat(MAX_INPUT - 4));
        let wide = format!("{}\u{20ac}after", "a".repeat(MAX_INPUT - 2));
        assert_eq!(to_text(&wide), "a".repeat(MAX_INPUT - 2));
    }

    #[test]
    fn inline_data_uris_and_long_style_attributes_are_not_text() {
        let image = format!(
            "a<img src=\"data:image/png;base64,{}\" alt=x>b",
            "QUJD".repeat(75_000)
        );
        for out in both(&image) {
            assert_eq!(out, "ab");
        }
        let style = format!("<p style=\"{}\">x</p>y", "color:red;".repeat(50_000));
        assert_eq!(to_text(&style), "x\n\ny");
        let css = format!("a<style>{}</style>b", "p{color:red}".repeat(100_000));
        assert_eq!(to_text(&css), "ab");
    }

    #[test]
    fn the_scanners_report_exact_lengths() {
        assert_eq!(tag_length("<a>"), Some(3));
        assert_eq!(tag_length("<a title=\">\">x"), Some(13));
        assert_eq!(tag_length("<a title='>' b=\"'\">"), Some(19));
        assert_eq!(tag_length("<a"), None);
        assert_eq!(tag_length("<a title=\"x>"), None);
        assert_eq!(tag_length(""), None);
        assert_eq!(comment("<!---->"), 7);
        assert_eq!(comment("<!-->x"), 5);
        assert_eq!(comment("<!--->x"), 6);
        assert_eq!(comment("<!-- x -->y"), 10);
        assert_eq!(comment("<!-- x"), 6);
        assert_eq!(comment("<!--"), 4);
        assert_eq!(to_close("<!x>y"), 4);
        assert_eq!(to_close("<!x"), 3);
        assert_eq!(raw_length("abc</script>d", "script"), Some(12));
        assert_eq!(raw_length("abc</SCRIPT >d", "script"), Some(13));
        assert_eq!(raw_length("abc</scriptx>d", "script"), None);
        assert_eq!(raw_length("abc</script", "script"), None);
        assert_eq!(raw_length("abc</script/", "script"), Some(12));
        assert_eq!(raw_length("abc", "script"), None);
        assert_eq!(decode("&amp;x"), Some(('&', 5)));
        assert_eq!(decode("&#65;"), Some(('A', 5)));
        assert_eq!(decode("&#x41;"), Some(('A', 6)));
        assert_eq!(decode("&amp"), None);
        assert_eq!(decode("&"), None);
        assert_eq!(decode("&;"), None);
        assert_eq!(decode("&#0000000065;"), Some(('A', 13)));
        assert_eq!(decode("&#00000000065;"), Some(('A', 14)));
        assert_eq!(decode("&#000000000065;"), None);
        assert_eq!(decode("&abcdefghijkl;"), None);
    }

    #[test]
    fn numeric_references_parse_strictly() {
        assert_eq!(numeric("65"), Some('A'));
        assert_eq!(numeric("x41"), Some('A'));
        assert_eq!(numeric("X41"), Some('A'));
        assert_eq!(numeric("0"), Some('\0'));
        assert_eq!(numeric("1114111"), Some('\u{10ffff}'));
        assert_eq!(numeric(""), None);
        assert_eq!(numeric("x"), None);
        assert_eq!(numeric("+65"), None);
        assert_eq!(numeric("-65"), None);
        assert_eq!(numeric("6 5"), None);
        assert_eq!(numeric("1114112"), None);
        assert_eq!(numeric("55296"), None);
        assert_eq!(numeric("xD800"), None);
        assert_eq!(numeric("4294967296"), None);
        assert_eq!(numeric("xFFFFFFFFFF"), None);
        assert_eq!(numeric("\u{665}"), None);
        assert_eq!(numeric("x\u{ff21}"), None);
    }

    #[test]
    fn hostile_documents_are_processed_in_linear_time() {
        let started = Instant::now();
        for hostile in [
            filled("<a ", HOSTILE),
            filled("<a b=\"", HOSTILE),
            filled("<a \"", HOSTILE),
            filled("<a '", HOSTILE),
            filled("<a>", HOSTILE),
            filled("<\"", HOSTILE),
            filled("<!--", HOSTILE),
            filled("<!", HOSTILE),
            filled("<?", HOSTILE),
            filled("<![CDATA[", HOSTILE),
            filled("<", HOSTILE),
            filled("</", HOSTILE),
            filled("</a", HOSTILE),
            filled("<div>", HOSTILE),
            filled("<p>", HOSTILE),
            filled("<td>", HOSTILE),
            filled("<pre>", HOSTILE),
            filled("<br>", HOSTILE),
            filled("<script>", HOSTILE),
            filled("<script></ ", HOSTILE),
            filled("<script></scriptx", HOSTILE),
            filled("<style>", HOSTILE),
            filled("<title>", HOSTILE),
            filled("<head>", HOSTILE),
            filled("<head></h", HOSTILE),
            filled("&amp", HOSTILE),
            filled("&", HOSTILE),
            filled("&#", HOSTILE),
            filled("&#x", HOSTILE),
            filled("&;", HOSTILE),
            filled("&yuml;", HOSTILE),
            filled("&notanentity;", HOSTILE),
            filled("\u{1f600}<", HOSTILE),
            filled("\u{0}", HOSTILE),
            filled("\u{200b}\u{202e}", HOSTILE),
            filled("\r", HOSTILE),
            filled("a<br>\r\n", HOSTILE),
            filled("a b\n", HOSTILE),
            format!("{}x", filled("<div>", HOSTILE)),
        ] {
            for out in both(&hostile) {
                assert!(well_formed(&out));
                assert!(out.len() <= hostile.len());
            }
        }
        assert!(started.elapsed() < Duration::from_secs(30));
    }

    #[test]
    fn hostile_documents_that_must_come_out_empty_do() {
        for piece in [
            "<a ",
            "<a b=\"",
            "<!--",
            "<script>",
            "<script></ ",
            "<script></scriptx",
            "<style>",
            "<title>",
            "<head>",
            "<!",
            "<![CDATA[",
            "<br>",
            "<p>",
            "\u{0}",
            "\u{200b}\u{202e}",
            "\r",
            " ",
            "&nbsp;",
        ] {
            for out in both(&filled(piece, HOSTILE)) {
                assert_eq!(out, "", "{piece:?}");
            }
        }
        for out in both(&format!("{}x", filled("<div>", HOSTILE))) {
            assert_eq!(out, "x");
        }
    }

    #[test]
    fn random_documents_keep_every_invariant() -> TestResult {
        const PIECES: &[&str] = &[
            "<",
            ">",
            "&",
            ";",
            "#",
            "x",
            "X",
            "\"",
            "'",
            "=",
            "/",
            "!",
            "?",
            "-",
            "--",
            " ",
            "  ",
            "\t",
            "\n",
            "\r",
            "\r\n",
            "a",
            "bc",
            "\u{e4}",
            "\u{20ac}",
            "\u{1f600}",
            "\u{0}",
            "\u{200b}",
            "\u{202e}",
            "\u{a0}",
            "&amp;",
            "&lt;",
            "&#65;",
            "&#x41;",
            "&#0;",
            "&nbsp;",
            "&#xD800;",
            "&#99999999999;",
            "<br>",
            "<p>",
            "</p>",
            "<div>",
            "</div>",
            "<td>",
            "<li>",
            "<pre>",
            "</pre>",
            "<a href=\"",
            "<a b='c'>",
            "</a>",
            "<!--",
            "-->",
            "<script>",
            "</script>",
            "<style>",
            "</style>",
            "<head>",
            "</head>",
            "<![CDATA[",
            "]]>",
            "<title>",
            "</title>",
            "<?",
            "<!",
            "<a ",
            "</a ",
            "&#",
            "&#x",
        ];
        let mut seed: u32 = 0x2545_f491;
        for _ in 0..4_000 {
            let mut document = String::new();
            for _ in 0..40 {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                let index = usize::try_from(seed >> 16)? % PIECES.len();
                document.push_str(PIECES.get(index).ok_or("piece")?);
            }
            for out in both(&document) {
                assert!(well_formed(&out), "{document:?} -> {out:?}");
                assert!(out.len() <= document.len(), "{document:?} -> {out:?}");
            }
        }
        Ok(())
    }

    const VISIBLE: &[(&str, &str)] = &[
        ("a", "a"),
        ("w\u{f6}rld", "w\u{f6}rld"),
        ("&amp;", "&"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&#65;", "A"),
        ("&#x42;", "B"),
        ("&euro;", "\u{20ac}"),
        ("it's", "it's"),
        ("3>2", "3>2"),
        ("\"q\"", "\"q\""),
        ("\u{1f600}", "\u{1f600}"),
        ("\u{65e5}\u{672c}", "\u{65e5}\u{672c}"),
    ];
    const SPACES: &[&str] = &[
        " ", "  ", "\n", "\r\n", "\t", "\r", "\u{a0}", "\u{2003}", " \n ", "\n\n\n", "&nbsp;",
    ];
    const INVISIBLE: &[&str] = &[
        "<b>",
        "</b>",
        "<a href=\"x>y\" title='a>b'>",
        "</a>",
        "<span class=c>",
        "<!-- <b>x</b> > -->",
        "<script>if (a < b) { x(); }</script>",
        "<style>p > a {}</style>",
        "<br>",
        "<p>",
        "</p>",
        "<div>",
        "</div>",
        "<ul><li>",
        "</li></ul>",
        "<td>",
        "<!DOCTYPE html>",
        "<img src=x alt=\"SECRET\">",
        "<o:p></o:p>",
        "<?xml version=\"1.0\"?>",
    ];

    struct Sample {
        pieces: Vec<(usize, usize)>,
        visible: String,
    }

    impl Sample {
        fn generate(seed: &mut u32) -> Result<Sample, Box<dyn std::error::Error>> {
            let mut pieces = Vec::new();
            let mut visible = String::new();
            for _ in 0..40 {
                *seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                let kind = usize::try_from(*seed >> 16)? % 3;
                *seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                let pick = usize::try_from(*seed >> 16)?;
                match kind {
                    0 => {
                        let index = pick % VISIBLE.len();
                        visible.push_str(VISIBLE.get(index).ok_or("visible")?.1);
                        pieces.push((0, index));
                    }
                    1 => pieces.push((1, pick % SPACES.len())),
                    _ => pieces.push((2, pick % INVISIBLE.len())),
                }
            }
            Ok(Sample { pieces, visible })
        }

        fn render(&self, shift: usize) -> Result<String, Box<dyn std::error::Error>> {
            let mut document = String::new();
            for (kind, index) in &self.pieces {
                let piece = match kind {
                    0 => VISIBLE.get(*index).ok_or("visible")?.0,
                    1 => SPACES.get((index + shift) % SPACES.len()).ok_or("space")?,
                    _ => INVISIBLE.get(*index).ok_or("invisible")?,
                };
                document.push_str(piece);
            }
            Ok(document)
        }
    }

    fn without_whitespace(text: &str) -> String {
        text.split_whitespace().collect()
    }

    #[test]
    fn generated_documents_keep_exactly_their_visible_text() -> TestResult {
        let mut seed: u32 = 0x1357_9bdf;
        for _ in 0..2_000 {
            let sample = Sample::generate(&mut seed)?;
            let document = sample.render(0)?;
            for out in both(&document) {
                assert_eq!(without_whitespace(&out), sample.visible, "{document:?}");
                assert!(!out.contains("SECRET"), "{document:?}");
                assert!(well_formed(&out), "{document:?} -> {out:?}");
            }
        }
        Ok(())
    }

    #[test]
    fn whitespace_runs_are_interchangeable_when_collapsing() -> TestResult {
        let mut seed: u32 = 0x2468_ace0;
        for _ in 0..2_000 {
            let sample = Sample::generate(&mut seed)?;
            let reference = to_text(&sample.render(0)?);
            for shift in 1..SPACES.len() {
                let variant = sample.render(shift)?;
                assert_eq!(to_text(&variant), reference, "{variant:?}");
            }
        }
        Ok(())
    }

    #[test]
    fn markup_never_leaks_into_the_output() -> TestResult {
        const WRAPPED: &[&str] = &[
            "<a href=\"SECRET\">",
            "<a href='SECRET'>",
            "<a href=SECRET>",
            "<img alt=\"SECRET\" src=x>",
            "<p data-x=\"a>SECRET\">",
            "<span title = 'SECRET' >",
            "<!-- SECRET -->",
            "<!--SECRET-->",
            "<!DOCTYPE SECRET>",
            "<?SECRET?>",
            "<![CDATA[SECRET]]>",
            "<script>SECRET</script>",
            "<style>SECRET</style>",
            "<title>SECRET</title>",
            "<head>SECRET</head>",
            "<SCRIPT>SECRET</SCRIPT >",
            "</SECRET>",
            "</ SECRET>",
        ];
        let mut seed: u32 = 99;
        for _ in 0..500 {
            let mut document = String::from("start ");
            for _ in 0..8 {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                let index = usize::try_from(seed >> 16)? % WRAPPED.len();
                document.push_str(WRAPPED.get(index).ok_or("wrapped")?);
                document.push_str(" text ");
            }
            for out in both(&document) {
                assert!(!out.contains("SECRET"), "{document:?} -> {out:?}");
                assert!(out.starts_with("start"));
            }
        }
        Ok(())
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
                for out in both(prefix) {
                    assert!(well_formed(&out), "{prefix:?}");
                    assert!(out.len() <= prefix.len(), "{prefix:?}");
                }
            }
        }
        for (position, original) in seed.char_indices() {
            for replacement in [
                "<", ">", "&", ";", "\"", "'", "=", "/", "!", "-", "\u{0}", "\u{e4}", "#", "",
                "\u{202e}", "\r",
            ] {
                let mut mutated = String::new();
                mutated.push_str(seed.get(..position).unwrap_or_default());
                mutated.push_str(replacement);
                mutated.push_str(
                    seed.get(position + original.len_utf8()..)
                        .unwrap_or_default(),
                );
                for out in both(&mutated) {
                    assert!(well_formed(&out), "{mutated:?}");
                    assert!(out.len() <= mutated.len(), "{mutated:?}");
                }
            }
        }
    }

    #[test]
    fn plain_text_without_markup_is_a_fixed_point() -> TestResult {
        for plain in [
            "",
            "plain",
            "two words",
            "line one\nline two",
            "Gr\u{fc}\u{df}e, \u{20ac}5 \u{2013} 1 > 0 and 2 >= 1",
            "a\n\nb",
            "tabs\tand  spaces",
        ] {
            let text = to_text(plain);
            assert_eq!(to_text(&text), text, "{plain:?}");
            let notes = to_notes(plain);
            assert_eq!(to_notes(&notes), notes, "{plain:?}");
        }
        let alphabet: Vec<char> =
            "ab xyz\n\t.,:;-_()[]{}!?\"'/\\*+=#%$@^~|`>\u{e4}\u{20ac}\u{1f600}\r\u{a0}"
                .chars()
                .collect();
        let mut seed: u32 = 4_711;
        for _ in 0..500 {
            let mut sample = String::new();
            for _ in 0..40 {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                let index = usize::try_from(seed >> 16)? % alphabet.len();
                sample.push(*alphabet.get(index).ok_or("alphabet")?);
            }
            let text = to_text(&sample);
            assert!(well_formed(&text), "{sample:?}");
            assert_eq!(to_text(&text), text, "{sample:?}");
            let notes = to_notes(&sample);
            assert!(well_formed(&notes), "{sample:?}");
            assert_eq!(to_notes(&notes), notes, "{sample:?}");
        }
        Ok(())
    }

    #[test]
    fn plain_text_survives_escaping_and_conversion() -> TestResult {
        for original in [
            "Q&A",
            "a < b > c",
            "<b>nicht fett</b>",
            "Anna <anna@example.org>",
            "&amp; &lt; &#65;",
            "Zeile 1\n\nZeile 3",
            "\"zitat\" 'x' \u{e4}\u{f6}\u{fc} \u{20ac}",
            "<script>alert(1)</script>",
            "<!-- kein kommentar -->",
            "1 < 2 && 3 > 2",
            "  eingerueckt\n\tmit tab",
        ] {
            let escaped = escape(original);
            assert!(
                !escaped.contains('<') && !escaped.contains('>'),
                "{escaped}"
            );
            let expected = original.trim();
            assert_eq!(to_notes(&escaped), expected, "{original}");
        }
        let mut seed: u32 = 12345;
        let alphabet: Vec<char> = "ab <>&;#x1 \n\"'/!-".chars().collect();
        for _ in 0..300 {
            let mut sample = String::new();
            for _ in 0..24 {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                let index = usize::try_from(seed >> 16)? % alphabet.len();
                sample.push(*alphabet.get(index).ok_or("alphabet")?);
            }
            let sample = to_notes(&sample);
            assert_eq!(to_notes(&escape(&sample)), sample, "{sample:?}");
        }
        Ok(())
    }

    #[test]
    fn escaping_covers_the_three_special_characters() {
        assert_eq!(escape("a&b<c>d\"e'f"), "a&amp;b&lt;c&gt;d\"e'f");
        assert_eq!(escape(""), "");
        assert_eq!(escape("plain \u{e4}\u{1f600}\n"), "plain \u{e4}\u{1f600}\n");
        assert_eq!(escape("&amp;"), "&amp;amp;");
        assert_eq!(escape("<<>>&&"), "&lt;&lt;&gt;&gt;&amp;&amp;");
    }
}
