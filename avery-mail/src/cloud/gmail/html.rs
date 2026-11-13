const MAX_TAG: usize = 4096;
const MAX_TAG_NAME: usize = 32;
const MAX_FAILED_TAGS: usize = 256;
const MAX_ENTITY: usize = 12;
const MAX_NUMERIC: usize = 7;

const ENTITIES: &[(&str, char)] = &[
    ("amp", '&'),
    ("lt", '<'),
    ("gt", '>'),
    ("quot", '"'),
    ("apos", '\''),
    ("nbsp", ' '),
    ("ensp", ' '),
    ("emsp", ' '),
    ("thinsp", ' '),
    ("shy", '\u{AD}'),
    ("zwnj", '\u{200C}'),
    ("zwj", '\u{200D}'),
    ("copy", '\u{A9}'),
    ("reg", '\u{AE}'),
    ("trade", '\u{2122}'),
    ("hellip", '\u{2026}'),
    ("mdash", '\u{2014}'),
    ("ndash", '\u{2013}'),
    ("lsquo", '\u{2018}'),
    ("rsquo", '\u{2019}'),
    ("sbquo", '\u{201A}'),
    ("ldquo", '\u{201C}'),
    ("rdquo", '\u{201D}'),
    ("bdquo", '\u{201E}'),
    ("laquo", '\u{AB}'),
    ("raquo", '\u{BB}'),
    ("lsaquo", '\u{2039}'),
    ("rsaquo", '\u{203A}'),
    ("bull", '\u{2022}'),
    ("middot", '\u{B7}'),
    ("euro", '\u{20AC}'),
    ("pound", '\u{A3}'),
    ("yen", '\u{A5}'),
    ("cent", '\u{A2}'),
    ("sect", '\u{A7}'),
    ("deg", '\u{B0}'),
    ("plusmn", '\u{B1}'),
    ("times", '\u{D7}'),
    ("divide", '\u{F7}'),
    ("para", '\u{B6}'),
    ("micro", '\u{B5}'),
    ("frac12", '\u{BD}'),
    ("frac14", '\u{BC}'),
    ("frac34", '\u{BE}'),
    ("iexcl", '\u{A1}'),
    ("iquest", '\u{BF}'),
    ("larr", '\u{2190}'),
    ("rarr", '\u{2192}'),
    ("auml", '\u{E4}'),
    ("ouml", '\u{F6}'),
    ("uuml", '\u{FC}'),
    ("Auml", '\u{C4}'),
    ("Ouml", '\u{D6}'),
    ("Uuml", '\u{DC}'),
    ("szlig", '\u{DF}'),
    ("eacute", '\u{E9}'),
    ("egrave", '\u{E8}'),
    ("agrave", '\u{E0}'),
    ("aacute", '\u{E1}'),
    ("acirc", '\u{E2}'),
    ("ccedil", '\u{E7}'),
];

struct Tag {
    name: String,
    closing: bool,
    length: usize,
}

enum Parsed {
    Literal,
    Unterminated,
    Found(Tag),
}

fn parse_tag(rest: &str) -> Parsed {
    let Some(body) = rest.strip_prefix('<') else {
        return Parsed::Literal;
    };
    let (closing, body) = match body.strip_prefix('/') {
        Some(after) => (true, after),
        None => (false, body),
    };
    let name: String = body
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .take(MAX_TAG_NAME)
        .collect();
    if !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return Parsed::Literal;
    }
    let bytes = rest.as_bytes();
    let mut index = usize::from(closing) + 1 + name.len();
    let mut quote: Option<u8> = None;
    let mut after_equals = false;
    while let Some(byte) = bytes.get(index).copied() {
        if index > MAX_TAG {
            return Parsed::Unterminated;
        }
        match quote {
            Some(open) => {
                if byte == open {
                    quote = None;
                }
            }
            None => match byte {
                b'>' => {
                    return Parsed::Found(Tag {
                        name: name.to_ascii_lowercase(),
                        closing,
                        length: index + 1,
                    })
                }
                b'"' | b'\'' if after_equals => {
                    quote = Some(byte);
                    after_equals = false;
                }
                b'=' => after_equals = true,
                other if other.is_ascii_whitespace() => {}
                _ => after_equals = false,
            },
        }
        index += 1;
    }
    Parsed::Unterminated
}

fn entity(rest: &str) -> Option<(char, usize)> {
    let end = rest
        .as_bytes()
        .iter()
        .take(MAX_ENTITY + 1)
        .position(|byte| *byte == b';')?;
    let name = rest.get(1..end)?;
    let length = end + 1;
    if let Some(numeric) = name.strip_prefix('#') {
        let (digits, radix) = match numeric.strip_prefix(['x', 'X']) {
            Some(hex) => (hex, 16),
            None => (numeric, 10),
        };
        if digits.is_empty()
            || digits.len() > MAX_NUMERIC
            || !digits.chars().all(|c| c.is_digit(radix))
        {
            return None;
        }
        let code = u32::from_str_radix(digits, radix).ok()?;
        let decoded = match char::from_u32(code) {
            Some('\0') | None => '\u{FFFD}',
            Some(other) => other,
        };
        return Some((decoded, length));
    }
    ENTITIES
        .iter()
        .find(|(known, _)| *known == name)
        .map(|(_, decoded)| (*decoded, length))
}

struct Converter<'a> {
    source: &'a str,
    lower: String,
    position: usize,
    text: String,
    space: bool,
    pre: usize,
    failed: usize,
}

impl<'a> Converter<'a> {
    fn new(source: &'a str) -> Converter<'a> {
        Converter {
            source,
            lower: source.to_ascii_lowercase(),
            position: 0,
            text: String::with_capacity(source.len()),
            space: false,
            pre: 0,
            failed: 0,
        }
    }

    fn rest(&self) -> &'a str {
        self.source.get(self.position..).unwrap_or_default()
    }

    fn word(&mut self, c: char) {
        if self.space && !self.text.is_empty() && !self.text.ends_with('\n') {
            self.text.push(' ');
        }
        self.space = false;
        self.text.push(c);
    }

    fn character(&mut self, c: char) {
        if self.pre > 0 {
            match c {
                '\r' => {}
                other => self.text.push(other),
            }
        } else if c.is_whitespace() {
            self.space = true;
        } else {
            self.word(c);
        }
    }

    fn breaks(&mut self, count: usize) {
        self.space = false;
        while self.text.ends_with(' ') {
            self.text.pop();
        }
        if self.text.is_empty() {
            return;
        }
        let existing = self
            .text
            .chars()
            .rev()
            .take_while(|c| *c == '\n')
            .take(count)
            .count();
        for _ in existing..count {
            self.text.push('\n');
        }
    }

    fn line_break(&mut self) {
        self.space = false;
        while self.text.ends_with(' ') {
            self.text.pop();
        }
        if self.text.is_empty() {
            return;
        }
        let trailing = self
            .text
            .chars()
            .rev()
            .take_while(|c| *c == '\n')
            .take(2)
            .count();
        if trailing < 2 {
            self.text.push('\n');
        }
    }

    fn skip_raw(&mut self, name: &str) {
        let closer = format!("</{name}");
        let end = match self
            .lower
            .get(self.position..)
            .and_then(|rest| rest.find(&closer))
        {
            Some(offset) => {
                let after = self.position + offset + closer.len();
                self.source
                    .get(after..)
                    .and_then(|rest| rest.find('>'))
                    .map_or(self.source.len(), |close| after + close + 1)
            }
            None => self.source.len(),
        };
        self.position = end;
    }

    fn apply(&mut self, tag: &Tag) {
        let name = tag.name.as_str();
        if !tag.closing && matches!(name, "script" | "style" | "title") {
            self.skip_raw(name);
            return;
        }
        match (name, tag.closing) {
            ("br", _) => self.line_break(),
            ("hr", _) => self.breaks(1),
            ("p", false) => self.breaks(1),
            ("p", true) => self.breaks(2),
            ("pre", false) => {
                self.breaks(1);
                self.pre += 1;
            }
            ("pre", true) => {
                self.pre = self.pre.saturating_sub(1);
                self.breaks(1);
            }
            ("td" | "th", _) => self.space = true,
            (
                "div" | "tr" | "li" | "ul" | "ol" | "dl" | "dt" | "dd" | "table" | "thead"
                | "tbody" | "tfoot" | "caption" | "blockquote" | "h1" | "h2" | "h3" | "h4" | "h5"
                | "h6" | "section" | "article" | "header" | "footer" | "nav" | "aside" | "main"
                | "address" | "form" | "fieldset" | "center" | "figure" | "figcaption" | "details"
                | "summary",
                _,
            ) => self.breaks(1),
            _ => {}
        }
    }

    fn markup(&mut self) {
        let rest = self.rest();
        if rest.starts_with("<!--") {
            self.position += rest.find("-->").map_or(rest.len(), |end| end + 3);
            return;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            self.position += rest.find('>').map_or(rest.len(), |end| end + 1);
            return;
        }
        let parsed = if self.failed >= MAX_FAILED_TAGS {
            Parsed::Literal
        } else {
            parse_tag(rest)
        };
        match parsed {
            Parsed::Found(tag) => {
                self.position += tag.length;
                self.apply(&tag);
            }
            Parsed::Unterminated => {
                self.failed += 1;
                self.position += 1;
                self.character('<');
            }
            Parsed::Literal => {
                self.position += 1;
                self.character('<');
            }
        }
    }

    fn reference(&mut self) {
        match entity(self.rest()) {
            Some((decoded, length)) => {
                self.position += length;
                self.character(decoded);
            }
            None => {
                self.position += 1;
                self.character('&');
            }
        }
    }

    fn run(&mut self) {
        let rest = self.rest();
        let length = rest.find(['<', '&']).unwrap_or(rest.len());
        for c in rest.get(..length).unwrap_or_default().chars() {
            self.character(c);
        }
        self.position += length.max(1);
    }

    fn convert(mut self) -> String {
        while let Some(byte) = self.source.as_bytes().get(self.position).copied() {
            match byte {
                b'<' => self.markup(),
                b'&' => self.reference(),
                _ => self.run(),
            }
        }
        self.text.trim().to_string()
    }
}

pub(crate) fn to_text(html: &str) -> String {
    Converter::new(html).convert()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_elements_and_breaks_become_line_breaks() {
        assert_eq!(to_text("<p>Hello</p><p>World</p>"), "Hello\n\nWorld");
        assert_eq!(
            to_text("Hello<br>World<BR/>again<br />!"),
            "Hello\nWorld\nagain\n!"
        );
        assert_eq!(to_text("<div>a</div><div>b</div>"), "a\nb");
        assert_eq!(to_text("<ul><li>one</li><li>two</li></ul>"), "one\ntwo");
        assert_eq!(
            to_text("<table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table>"),
            "a b\nc d"
        );
        assert_eq!(to_text("<h1>Title</h1>text<hr>more"), "Title\ntext\nmore");
        assert_eq!(to_text("a<br><br><br><br>b"), "a\n\nb");
        assert_eq!(to_text("<p></p><p></p>x<p></p>"), "x");
        assert_eq!(to_text("<blockquote>q</blockquote>after"), "q\nafter");
    }

    #[test]
    fn tags_are_stripped_and_their_text_is_kept() {
        assert_eq!(
            to_text("<html><head><meta charset=\"utf-8\"></head><body><b>bold</b> and <a href=\"https://x.example/?a=1&b=2\">a link</a></body></html>"),
            "bold and a link"
        );
        assert_eq!(to_text("<img src=\"x.png\" alt=\"pic\">text"), "text");
        assert_eq!(to_text("<o:p></o:p>Outlook<o:p>&nbsp;</o:p>"), "Outlook");
        assert_eq!(to_text("<A HREF='x'>Up</A><SPAN>Per</SPAN>"), "UpPer");
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
    }

    #[test]
    fn comments_and_declarations_are_dropped() {
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
    }

    #[test]
    fn entities_are_decoded_once() {
        assert_eq!(
            to_text("Tom &amp; Jerry &lt;3 &gt; &quot;q&quot; &apos;a&apos;"),
            "Tom & Jerry <3 > \"q\" 'a'"
        );
        assert_eq!(to_text("&amp;lt; &amp;amp;"), "&lt; &amp;");
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
        assert_eq!(to_text("a&nbsp;&nbsp;b"), "a b");
        assert_eq!(
            to_text("&lsquo;x&rsquo; &ldquo;y&rdquo;"),
            "\u{2018}x\u{2019} \u{201c}y\u{201d}"
        );
    }

    #[test]
    fn unknown_or_invalid_references_stay_literal() {
        assert_eq!(
            to_text("&unknown; &amp &#; &#x; &#xZZ; &#12a; & ;"),
            "&unknown; &amp &#; &#x; &#xZZ; &#12a; & ;"
        );
        assert_eq!(to_text("AT&T and R&D"), "AT&T and R&D");
        assert_eq!(to_text("&#99999999999;"), "&#99999999999;");
        assert_eq!(to_text("&#1114112;"), "\u{fffd}");
        assert_eq!(to_text("&#55296;"), "\u{fffd}");
        assert_eq!(to_text("a&#0;b"), "a\u{fffd}b");
        assert_eq!(
            to_text("&averyveryverylongentityname;"),
            "&averyveryverylongentityname;"
        );
        assert_eq!(to_text("&"), "&");
        assert_eq!(to_text("&#"), "&#");
    }

    #[test]
    fn whitespace_is_collapsed_like_a_browser_does() {
        assert_eq!(to_text("  a \n\t b   c  "), "a b c");
        assert_eq!(to_text("<p>  a  </p>   <p>  b  </p>"), "a\n\nb");
        assert_eq!(to_text("a\u{a0}b"), "a b");
        assert_eq!(to_text("a <b>b</b> c"), "a b c");
        assert_eq!(to_text("a<b> b </b>c"), "a b c");
        assert_eq!(to_text("line one\r\nline two"), "line one line two");
    }

    #[test]
    fn preformatted_text_keeps_its_lines() {
        assert_eq!(
            to_text("<pre>a\n  b\r\n\nc</pre>after"),
            "a\n  b\n\nc\nafter"
        );
        assert_eq!(to_text("x<pre>\tq &amp; r</pre>"), "x\n\tq & r");
        assert_eq!(to_text("</pre></pre>a"), "a");
        assert_eq!(to_text("<pre>a<pre>b</pre>c</pre>d"), "a\nb\nc\nd");
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
    }

    #[test]
    fn quoted_attribute_values_may_contain_the_closing_bracket() {
        assert_eq!(to_text("<a title=\"a>b\">x</a>"), "x");
        assert_eq!(to_text("<a title='a>b'>x</a>"), "x");
        assert_eq!(to_text("<p don't>x</p>"), "x");
        assert_eq!(to_text("<a title = \"a>b\" href=x>y</a>"), "y");
    }

    #[test]
    fn unterminated_tags_become_text_and_never_hang() {
        assert_eq!(to_text("a <b unterminated"), "a <b unterminated");
        assert_eq!(to_text("a <a href=\"x>text"), "a <a href=\"x>text");
        let many = "<a ".repeat(300_000);
        let out = to_text(&many);
        assert!(out.starts_with("<a <a"));
        let long_tag = format!("x<a {}>y", "q".repeat(MAX_TAG + 10));
        assert!(to_text(&long_tag).starts_with("x<a qqq"));
    }

    #[test]
    fn deep_nesting_and_huge_inputs_are_handled_iteratively() {
        let deep = format!("{}x{}", "<div>".repeat(200_000), "</div>".repeat(200_000));
        assert_eq!(to_text(&deep), "x");
        let big = "<p>word</p>".repeat(100_000);
        let out = to_text(&big);
        assert_eq!(out.lines().filter(|line| *line == "word").count(), 100_000);
        let entities = "&amp;".repeat(500_000);
        assert_eq!(to_text(&entities).chars().count(), 500_000);
        assert_eq!(to_text(""), "");
        assert_eq!(to_text("   "), "");
    }

    #[test]
    fn multibyte_text_around_tags_is_kept_intact() {
        assert_eq!(
            to_text("Gr\u{fc}\u{df}e <b>\u{20ac}</b>\u{1f600}<br>\u{e4}"),
            "Gr\u{fc}\u{df}e \u{20ac}\u{1f600}\n\u{e4}"
        );
        assert_eq!(
            to_text("\u{4e2d}\u{6587}&amp;\u{65e5}\u{672c}"),
            "\u{4e2d}\u{6587}&\u{65e5}\u{672c}"
        );
        assert_eq!(to_text("<p title=\"\u{e4}\">\u{e4}</p>"), "\u{e4}");
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
            "Hallo Anna,\nDeine Rechnung \u{fc}ber 12,50 \u{20ac} ist da.\n\nJetzt bezahlen\n\nGr\u{fc}\u{df}e\nDein Team"
        );
    }
}
