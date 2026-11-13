use crate::text::take_chars;

const MAX_TAG: usize = 2048;
const MAX_ENTITY: usize = 12;
const MAX_NAME: usize = 16;
const ENTITIES: &[(&str, char)] = &[
    ("amp", '&'),
    ("lt", '<'),
    ("gt", '>'),
    ("quot", '"'),
    ("apos", '\''),
    ("nbsp", ' '),
    ("ndash", '\u{2013}'),
    ("mdash", '\u{2014}'),
    ("hellip", '\u{2026}'),
    ("bull", '\u{2022}'),
    ("middot", '\u{b7}'),
    ("lsquo", '\u{2018}'),
    ("rsquo", '\u{2019}'),
    ("ldquo", '\u{201c}'),
    ("rdquo", '\u{201d}'),
    ("laquo", '\u{ab}'),
    ("raquo", '\u{bb}'),
    ("copy", '\u{a9}'),
    ("reg", '\u{ae}'),
    ("trade", '\u{2122}'),
    ("euro", '\u{20ac}'),
    ("pound", '\u{a3}'),
    ("yen", '\u{a5}'),
    ("cent", '\u{a2}'),
    ("sect", '\u{a7}'),
    ("deg", '\u{b0}'),
    ("times", '\u{d7}'),
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

pub(super) fn escape(text: &str) -> String {
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

pub(super) fn to_text(html: &str, limit: usize) -> String {
    let mut out = String::new();
    let mut pos = 0;
    while let Some(rest) = html.get(pos..) {
        let Some(c) = rest.chars().next() else {
            break;
        };
        pos += match c {
            '<' => tag(rest, &mut out),
            '&' => entity(rest, &mut out),
            other => {
                out.push(other);
                other.len_utf8()
            }
        };
    }
    tidy(&out, limit)
}

fn line_break(out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "ul"
            | "ol"
            | "tr"
            | "table"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "blockquote"
            | "pre"
            | "hr"
            | "section"
            | "article"
            | "header"
            | "footer"
    )
}

fn starts_tag(body: &str) -> bool {
    let mut chars = body.chars();
    match (chars.next(), chars.next()) {
        (Some(first), _) if first.is_ascii_alphabetic() || first == '!' || first == '?' => true,
        (Some('/'), Some(second)) => second.is_ascii_alphabetic(),
        _ => false,
    }
}

fn tag_inner(rest: &str) -> Option<&str> {
    let body = rest.get(1..)?;
    if !starts_tag(body) {
        return None;
    }
    let mut quote: Option<char> = None;
    for (index, c) in body.char_indices() {
        if index >= MAX_TAG {
            return None;
        }
        match (quote, c) {
            (_, '<') => return None,
            (Some(open), c) if c == open => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '>') => return body.get(..index),
            (None, _) => {}
        }
    }
    None
}

fn tag_name(inner: &str) -> (bool, String) {
    let (closing, body) = match inner.strip_prefix('/') {
        Some(rest) => (true, rest),
        None => (false, inner),
    };
    let name = body
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .take(MAX_NAME)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    (closing, name)
}

fn skip_to_close(rest: &str, from: usize, name: &str) -> usize {
    let needle = format!("</{name}");
    let body = rest.get(from..).unwrap_or_default();
    body.as_bytes()
        .windows(needle.len().max(1))
        .position(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
        .map_or(rest.len(), |offset| from + offset)
}

fn tag(rest: &str, out: &mut String) -> usize {
    if rest.starts_with("<!--") {
        return rest.find("-->").map_or(rest.len(), |end| end + 3);
    }
    let Some(inner) = tag_inner(rest) else {
        out.push('<');
        return 1;
    };
    let used = inner.len() + 2;
    let (closing, name) = tag_name(inner);
    match (closing, name.as_str()) {
        (false, "script" | "style") => return skip_to_close(rest, used, &name),
        (_, "br") => out.push('\n'),
        (false, "li") => {
            line_break(out);
            out.push_str("- ");
        }
        (_, "li") => line_break(out),
        (_, block) if is_block(block) => line_break(out),
        (true, "td" | "th") => out.push(' '),
        _ => {}
    }
    used
}

fn numeric(code: u32) -> Option<char> {
    match code {
        0 => None,
        9 | 10 => char::from_u32(code),
        1..=31 | 127 => Some(' '),
        _ => char::from_u32(code),
    }
}

fn decode(name: &str) -> Option<char> {
    if let Some(number) = name.strip_prefix('#') {
        let code = match number.strip_prefix(['x', 'X']) {
            Some(hex) if !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()) => {
                u32::from_str_radix(hex, 16).ok()?
            }
            Some(_) => return None,
            None if !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()) => {
                number.parse::<u32>().ok()?
            }
            None => return None,
        };
        return numeric(code);
    }
    ENTITIES
        .iter()
        .find(|(known, _)| *known == name)
        .map(|(_, decoded)| *decoded)
}

fn entity(rest: &str, out: &mut String) -> usize {
    let decoded = rest.get(1..).and_then(|after| {
        let end = after
            .char_indices()
            .take(MAX_ENTITY + 1)
            .find(|(_, c)| *c == ';')
            .map(|(index, _)| index)?;
        let name = after.get(..end)?;
        Some((decode(name)?, end + 2))
    });
    match decoded {
        Some((c, used)) => {
            out.push(c);
            used
        }
        None => {
            out.push('&');
            1
        }
    }
}

fn tidy(raw: &str, limit: usize) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut blank = 0usize;
    for line in raw.replace("\r\n", "\n").replace('\r', "\n").split('\n') {
        let line = line.trim_end_matches([' ', '\t']);
        if line.is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    take_chars(out.trim(), limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestResult;

    fn text(html: &str) -> String {
        to_text(html, 10_000)
    }

    #[test]
    fn plain_text_passes_through_with_its_line_breaks() {
        assert_eq!(text("Bitte Kuchen mitbringen."), "Bitte Kuchen mitbringen.");
        assert_eq!(
            text("Zeile 1\nZeile 2\r\nZeile 3"),
            "Zeile 1\nZeile 2\nZeile 3"
        );
        assert_eq!(text("  \n oben\n\n\n\n unten \n  "), "oben\n\n unten");
        assert_eq!(text(""), "");
        assert_eq!(text("   \n\t "), "");
    }

    #[test]
    fn line_breaks_come_from_br_and_block_elements() {
        assert_eq!(text("a<br>b<br/>c<BR />d"), "a\nb\nc\nd");
        assert_eq!(text("a<br><br>b"), "a\n\nb");
        assert_eq!(text("<p>eins</p><p>zwei</p>"), "eins\nzwei");
        assert_eq!(text("<div>eins</div><div>zwei</div>"), "eins\nzwei");
        assert_eq!(text("vor<div>mitte</div>nach"), "vor\nmitte\nnach");
        assert_eq!(
            text("<ul><li>Brot</li><li>Milch</li></ul>Ende"),
            "- Brot\n- Milch\nEnde"
        );
        assert_eq!(text("<h1>Titel</h1>Text"), "Titel\nText");
        assert_eq!(
            text("<table><tr><td>a</td><td>b</td></tr><tr><td>c</td></tr></table>"),
            "a b\nc"
        );
    }

    #[test]
    fn other_tags_are_stripped_and_their_text_is_kept() {
        assert_eq!(
            text(
                r#"Siehe <a href="https://example.org/x?a=1&amp;b=2" target="_blank">hier</a> und <b>fett</b> <i>kursiv</i>"#
            ),
            "Siehe hier und fett kursiv"
        );
        assert_eq!(text("<span style='color: red'>rot</span>"), "rot");
        assert_eq!(text(r#"<a title="a>b">Text</a>"#), "Text");
        assert_eq!(text("<img src=x alt=y>Bild"), "Bild");
        assert_eq!(text("<!DOCTYPE html><html><body>x</body></html>"), "x");
    }

    #[test]
    fn script_style_and_comments_are_dropped() {
        assert_eq!(
            text("a<script>alert('x'); if (a < b) {}</script>b<style>p { color: red }</style>c"),
            "abc"
        );
        assert_eq!(text("a<SCRIPT type=x>evil()</ScRiPt>b"), "ab");
        assert_eq!(text("a<script>never closed and <b>more"), "a");
        assert_eq!(text("a<!-- hidden <b>note</b> -->b"), "ab");
        assert_eq!(text("a<!-- never closed"), "a");
        assert_eq!(text("a<!---->b"), "ab");
    }

    #[test]
    fn entities_are_decoded_and_unknown_ones_stay() {
        assert_eq!(
            text("Q&amp;A &lt;b&gt; &quot;x&quot; &apos;y&apos; a&nbsp;b"),
            "Q&A <b> \"x\" 'y' a b"
        );
        assert_eq!(
            text("&#65;&#x42;&#X43; &#8364; &euro;"),
            "ABC \u{20ac} \u{20ac}"
        );
        assert_eq!(text("&uuml;&Ouml;&szlig;"), "\u{fc}\u{d6}\u{df}");
        assert_eq!(
            text("&unknown; &amp &; &#; &#x; &#xZZ; &#99999999999;"),
            "&unknown; &amp &; &#; &#x; &#xZZ; &#99999999999;"
        );
        assert_eq!(text("&#0; &#xD800; &#1114112;"), "&#0; &#xD800; &#1114112;");
        assert_eq!(text("a&#13;b&#7;c"), "a b c");
        assert_eq!(text("&amp;lt;"), "&lt;");
        assert_eq!(text("&&&&amp;"), "&&&&");
        assert_eq!(text("&+41; &#+65;"), "&+41; &#+65;");
    }

    #[test]
    fn a_lone_angle_bracket_is_text_but_an_open_tag_is_not() {
        assert_eq!(text("a < b and c > d"), "a < b and c > d");
        assert_eq!(text("1<2"), "1<2");
        assert_eq!(text("x <3 y"), "x <3 y");
        assert_eq!(text("a<b and c>d"), "ad");
        assert_eq!(text("tail <b"), "tail <b");
        assert_eq!(text("a<b<c>d"), "a<bd");
        assert_eq!(text(r#"<a title="x<y">t</a>"#), r#"<a title="x<y">t"#);
        assert_eq!(text("</ x>y"), "</ x>y");
        let long = format!("<a {}>x", "y".repeat(MAX_TAG + 10));
        assert!(text(&long).starts_with("<a yyyy"));
    }

    #[test]
    fn the_result_is_cut_at_the_limit_in_characters() {
        let long = "\u{e4}".repeat(50);
        assert_eq!(to_text(&long, 10).chars().count(), 10);
        assert_eq!(to_text("abc", 3), "abc");
        assert_eq!(to_text("abcd", 3), "abc");
        assert_eq!(to_text("<p>abcd</p>", 0), "");
    }

    #[test]
    fn hostile_input_is_linear_and_never_panics() {
        for hostile in [
            "<!--".repeat(50_000),
            "&".repeat(200_000),
            "<a ".repeat(50_000),
            "<a>".repeat(100_000),
            "<script>".repeat(30_000),
            "</".repeat(100_000),
            "&#".repeat(100_000),
            "\u{1f600}<".repeat(30_000),
            "<br>".repeat(100_000),
            "<a \"".repeat(100_000),
            "<a '".repeat(100_000),
        ] {
            let converted = text(&hostile);
            assert!(converted.len() <= hostile.len() + 1);
        }
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
        ] {
            let escaped = escape(original);
            assert!(
                !escaped.contains('<') && !escaped.contains('>'),
                "{escaped}"
            );
            assert_eq!(text(&escaped), original, "{original}");
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
            let sample = tidy(&sample, 10_000);
            assert_eq!(text(&escape(&sample)), sample, "{sample:?}");
        }
        Ok(())
    }

    #[test]
    fn escaping_covers_the_three_special_characters() {
        assert_eq!(escape("a&b<c>d\"e'f"), "a&amp;b&lt;c&gt;d\"e'f");
        assert_eq!(escape(""), "");
    }
}
