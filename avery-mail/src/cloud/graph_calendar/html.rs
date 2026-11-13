const MAX_TAG: usize = 16;
const MAX_ENTITY: usize = 12;
const SKIPPED: &[&str] = &["script", "style", "head", "title"];
const PARAGRAPHS: &[&str] = &["p", "h1", "h2", "h3", "h4", "h5", "h6"];
const CELLS: &[&str] = &["td", "th"];
const BLOCKS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "dd",
    "details",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "header",
    "hr",
    "li",
    "main",
    "nav",
    "ol",
    "pre",
    "section",
    "table",
    "tr",
    "ul",
];
const NAMED: &[(&str, char)] = &[
    ("amp", '&'),
    ("lt", '<'),
    ("gt", '>'),
    ("quot", '"'),
    ("apos", '\''),
    ("nbsp", '\u{a0}'),
    ("ndash", '\u{2013}'),
    ("mdash", '\u{2014}'),
    ("hellip", '\u{2026}'),
    ("bull", '\u{2022}'),
    ("middot", '\u{b7}'),
    ("copy", '\u{a9}'),
    ("reg", '\u{ae}'),
    ("trade", '\u{2122}'),
    ("euro", '\u{20ac}'),
    ("laquo", '\u{ab}'),
    ("raquo", '\u{bb}'),
    ("lsquo", '\u{2018}'),
    ("rsquo", '\u{2019}'),
    ("ldquo", '\u{201c}'),
    ("rdquo", '\u{201d}'),
    ("auml", '\u{e4}'),
    ("ouml", '\u{f6}'),
    ("uuml", '\u{fc}'),
    ("Auml", '\u{c4}'),
    ("Ouml", '\u{d6}'),
    ("Uuml", '\u{dc}'),
    ("szlig", '\u{df}'),
    ("eacute", '\u{e9}'),
    ("egrave", '\u{e8}'),
    ("agrave", '\u{e0}'),
    ("ccedil", '\u{e7}'),
];

struct Writer {
    out: String,
    count: usize,
    limit: usize,
    space: bool,
    breaks: u8,
}

impl Writer {
    fn new(limit: usize) -> Writer {
        Writer {
            out: String::new(),
            count: 0,
            limit,
            space: false,
            breaks: 0,
        }
    }

    fn full(&self) -> bool {
        self.count > self.limit
    }

    fn put(&mut self, c: char) {
        if c.is_whitespace() {
            self.space = true;
            return;
        }
        if self.full() {
            return;
        }
        if !self.out.is_empty() {
            if self.breaks > 0 {
                for _ in 0..self.breaks {
                    self.out.push('\n');
                }
                self.count += usize::from(self.breaks);
            } else if self.space {
                self.out.push(' ');
                self.count += 1;
            }
        }
        self.breaks = 0;
        self.space = false;
        self.out.push(c);
        self.count += 1;
    }

    fn break_lines(&mut self, lines: u8) {
        self.breaks = self.breaks.max(lines);
    }

    fn newline(&mut self) {
        self.breaks = (self.breaks + 1).min(2);
    }

    fn gap(&mut self) {
        self.space = true;
    }
}

pub(super) fn to_text(html: &str, limit: usize) -> String {
    let mut writer = Writer::new(limit);
    let mut at = 0;
    while !writer.full() {
        let Some(c) = html.get(at..).and_then(|rest| rest.chars().next()) else {
            break;
        };
        at = match c {
            '<' => tag(html, at, &mut writer),
            '&' => entity(html, at, &mut writer),
            _ => {
                writer.put(c);
                at + c.len_utf8()
            }
        };
    }
    writer.out
}

fn tag_name(text: &str) -> String {
    if !text.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return String::new();
    }
    text.chars()
        .take_while(char::is_ascii_alphanumeric)
        .take(MAX_TAG + 1)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn tag_end(html: &str, from: usize) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (offset, c) in html.get(from..)?.char_indices() {
        match (quote, c) {
            (None, '>') => return Some(from + offset + 1),
            (None, '"' | '\'') => quote = Some(c),
            (Some(open), _) if c == open => quote = None,
            _ => {}
        }
    }
    None
}

fn tag(html: &str, at: usize, writer: &mut Writer) -> usize {
    let after = at + 1;
    let rest = html.get(after..).unwrap_or_default();
    if let Some(body) = rest.strip_prefix("!--") {
        return body
            .find("-->")
            .map_or(html.len(), |end| after + 3 + end + 3);
    }
    let closing = rest.starts_with('/');
    let name = tag_name(html.get(after + usize::from(closing)..).unwrap_or_default());
    if name.is_empty() {
        return match rest.chars().next() {
            Some('!' | '?') => tag_end(html, after).unwrap_or(html.len()),
            _ => {
                writer.put('<');
                after
            }
        };
    }
    let end = tag_end(html, after).unwrap_or(html.len());
    if !closing && SKIPPED.contains(&name.as_str()) {
        return skip(html, end, &name);
    }
    if name == "br" {
        writer.newline();
    } else if PARAGRAPHS.contains(&name.as_str()) {
        writer.break_lines(2);
    } else if BLOCKS.contains(&name.as_str()) {
        writer.break_lines(1);
    } else if CELLS.contains(&name.as_str()) {
        writer.gap();
    }
    end
}

fn skip(html: &str, from: usize, name: &str) -> usize {
    let found = html.get(from..).and_then(|rest| {
        rest.match_indices("</").find_map(|(offset, _)| {
            let head = rest.get(offset + 2..)?.get(..name.len())?;
            head.eq_ignore_ascii_case(name).then_some(offset)
        })
    });
    match found {
        Some(offset) => tag_end(html, from + offset + 1).unwrap_or(html.len()),
        None if name == "head" => from,
        None => html.len(),
    }
}

fn decode(name: &str) -> Option<char> {
    if let Some(number) = name.strip_prefix('#') {
        let value = match number.strip_prefix(['x', 'X']) {
            Some(hex) if !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()) => {
                u32::from_str_radix(hex, 16).ok()
            }
            Some(_) => None,
            None if !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()) => {
                number.parse::<u32>().ok()
            }
            None => None,
        }?;
        return char::from_u32(value).filter(|c| !c.is_control() || c.is_ascii_whitespace());
    }
    NAMED
        .iter()
        .find(|(entity, _)| *entity == name)
        .map(|(_, c)| *c)
}

fn entity(html: &str, at: usize, writer: &mut Writer) -> usize {
    let rest = html.get(at + 1..).unwrap_or_default();
    let found = rest
        .char_indices()
        .take(MAX_ENTITY + 1)
        .find(|(_, c)| *c == ';')
        .and_then(|(end, _)| Some((decode(rest.get(..end)?)?, end)));
    match found {
        Some((c, end)) => {
            writer.put(c);
            at + 1 + end + 1
        }
        None => {
            writer.put('&');
            at + 1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(html: &str) -> String {
        to_text(html, 10_000)
    }

    #[test]
    fn blocks_and_breaks_become_lines() {
        assert_eq!(text("<div>one</div><div>two</div>"), "one\ntwo");
        assert_eq!(text("<p>a</p><p>b</p>"), "a\n\nb");
        assert_eq!(text("a<br>b<br><br>c<br/>d"), "a\nb\n\nc\nd");
        assert_eq!(text("<ul><li>x</li><li>y</li></ul>"), "x\ny");
        assert_eq!(text("<table><tr><td>a</td><td>b</td></tr></table>"), "a b");
        assert_eq!(text("<h1>Head</h1>text"), "Head\n\ntext");
        assert_eq!(text("<div><br><br><br><br>x</div>"), "x");
        assert_eq!(text("x<br><br><br><br>y"), "x\n\ny");
    }

    #[test]
    fn whitespace_collapses_and_the_ends_are_trimmed() {
        assert_eq!(text("  \n <b>hello</b>   \t world \n "), "hello world");
        assert_eq!(text("a\r\n\r\n   b"), "a b");
        assert_eq!(text("<div>  a  </div>\n<div>  b  </div>"), "a\nb");
        assert_eq!(text(""), "");
        assert_eq!(text("   "), "");
        assert_eq!(text("<br><br>"), "");
    }

    #[test]
    fn entities_are_decoded_and_unknown_ones_stay() {
        assert_eq!(
            text("Tom &amp; Jerry &lt;b&gt; &quot;q&quot; &apos;s&apos;"),
            "Tom & Jerry <b> \"q\" 's'"
        );
        assert_eq!(
            text("&#65;&#x42;&#X43;&#252;&#x1F600;"),
            "ABC\u{fc}\u{1f600}"
        );
        assert_eq!(text("a&nbsp;&nbsp;b"), "a b");
        assert_eq!(
            text("&auml;&ouml;&uuml;&Auml;&szlig;&euro;&ndash;&hellip;"),
            "\u{e4}\u{f6}\u{fc}\u{c4}\u{df}\u{20ac}\u{2013}\u{2026}"
        );
        assert_eq!(text("AT&T &unknown; & done &"), "AT&T &unknown; & done &");
        assert_eq!(
            text("&#0; &#xD800; &#; &#x; &#99999999999;"),
            "&#0; &#xD800; &#; &#x; &#99999999999;"
        );
        assert_eq!(text("&#+65; &#-1; &#x+41;"), "&#+65; &#-1; &#x+41;");
        assert_eq!(
            text("&averyveryverylongentityname;"),
            "&averyveryverylongentityname;"
        );
        assert_eq!(
            text("&lt;script&gt;alert(1)&lt;/script&gt;"),
            "<script>alert(1)</script>"
        );
    }

    #[test]
    fn scripts_styles_heads_titles_and_comments_are_dropped() {
        assert_eq!(
            text("a<script>var x = '<b>';</script>b<style>p {color: red}</style>c"),
            "abc"
        );
        assert_eq!(text("a<!-- hidden <b>x</b> -->b"), "ab");
        assert_eq!(text("a<!-- never closed <b>x</b>"), "a");
        assert_eq!(
            text("<html><head><title>T</title><meta charset=\"utf-8\"></head><body>Body</body></html>"),
            "Body"
        );
        assert_eq!(text("<!DOCTYPE html><html><body>x</body></html>"), "x");
        assert_eq!(text("<?xml version=\"1.0\"?>x"), "x");
        assert_eq!(text("a<SCRIPT>1</ScRiPt>b"), "ab");
        assert_eq!(text("a<script>never closed</b>"), "a");
        assert_eq!(text("<head><body>kept</body>"), "kept");
        assert_eq!(text("a<style>x</style >b"), "ab");
    }

    #[test]
    fn a_real_outlook_body_becomes_plain_lines() {
        let html = concat!(
            "<html><head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=utf-8\">",
            "<style type=\"text/css\" style=\"display:none\">P {margin-top:0;margin-bottom:0;}</style></head>",
            "<body dir=\"ltr\"><div style=\"font-family: Calibri, Arial; font-size: 12pt; color: rgb(0, 0, 0);\">",
            "Bring the <b>blue</b> folder.<br>Room &amp; coffee at <a href=\"https://example.invalid/x?a=1&b=2\">the desk</a>.",
            "</div><div><br></div><div>Thanks!</div></body></html>"
        );
        assert_eq!(
            text(html),
            "Bring the blue folder.\nRoom & coffee at the desk.\n\nThanks!"
        );
    }

    #[test]
    fn tags_are_matched_loosely_and_quotes_may_hide_a_closing_bracket() {
        assert_eq!(text("<a title=\"x>y\">link</a>"), "link");
        assert_eq!(text("<a title='x>y'>link</a>"), "link");
        assert_eq!(text("a <b and c"), "a");
        assert_eq!(text("1 < 2 and 3 > 2"), "1 < 2 and 3 > 2");
        assert_eq!(text("a<3"), "a<3");
        assert_eq!(
            text("<verylongtagnamethatmakesnosense>x</verylongtagnamethatmakesnosense>"),
            "x"
        );
        assert_eq!(text("<DIV>A</DIV><Div>B</Div>"), "A\nB");
        assert_eq!(text("<span>a</span><span>b</span>"), "ab");
    }

    #[test]
    fn unicode_and_invisible_characters_are_kept() {
        assert_eq!(
            text("<p>caf\u{e9} \u{1f600} a\u{200b}b \u{202e}x</p>"),
            "caf\u{e9} \u{1f600} a\u{200b}b \u{202e}x"
        );
        assert_eq!(text("a\u{0}b"), "a\u{0}b");
    }

    #[test]
    fn the_output_is_bounded_whatever_the_input() {
        let long = "word ".repeat(50_000);
        let out = to_text(&long, 100);
        assert!(out.chars().count() <= 104, "{}", out.chars().count());
        assert!(out.starts_with("word word"));
        let lines = "<div>x</div>".repeat(50_000);
        assert!(to_text(&lines, 10).chars().count() <= 14);
        assert_eq!(to_text("abc", 0).chars().count(), 1);
    }

    #[test]
    fn pathological_markup_terminates() {
        let started = std::time::Instant::now();
        for hostile in [
            "<".repeat(200_000),
            "&".repeat(200_000),
            "<a ".repeat(100_000),
            "<script>".repeat(100_000),
            "<!--".repeat(100_000),
            "<\"".repeat(100_000),
            "&#".repeat(100_000),
        ] {
            let _ = to_text(&hostile, 1_000);
        }
        assert!(started.elapsed() < std::time::Duration::from_secs(20));
    }
}
