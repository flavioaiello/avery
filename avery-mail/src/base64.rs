const STANDARD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URL_SAFE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn encode_with(data: &[u8], alphabet: &[u8; 64], padded: bool) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    let symbol = |index: u32| -> char {
        usize::try_from(index & 63)
            .ok()
            .and_then(|position| alphabet.get(position))
            .map_or('A', |byte| char::from(*byte))
    };
    for chunk in data.chunks(3) {
        let first = u32::from(chunk.first().copied().unwrap_or(0));
        let second = u32::from(chunk.get(1).copied().unwrap_or(0));
        let third = u32::from(chunk.get(2).copied().unwrap_or(0));
        let block = (first << 16) | (second << 8) | third;
        out.push(symbol(block >> 18));
        out.push(symbol(block >> 12));
        if chunk.len() > 1 {
            out.push(symbol(block >> 6));
        } else if padded {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(symbol(block));
        } else if padded {
            out.push('=');
        }
    }
    out
}

pub(crate) fn encode(data: &[u8]) -> String {
    encode_with(data, STANDARD, true)
}

pub(crate) fn encode_url(data: &[u8]) -> String {
    encode_with(data, URL_SAFE, false)
}

fn value_of(byte: u8) -> Option<u32> {
    match byte {
        b'A'..=b'Z' => Some(u32::from(byte - b'A')),
        b'a'..=b'z' => Some(u32::from(byte - b'a') + 26),
        b'0'..=b'9' => Some(u32::from(byte - b'0') + 52),
        b'+' | b'-' => Some(62),
        b'/' | b'_' => Some(63),
        _ => None,
    }
}

pub(crate) fn decode(text: &str) -> Option<Vec<u8>> {
    let mut symbols: Vec<u32> = Vec::with_capacity(text.len());
    let mut padding = 0usize;
    for byte in text.bytes() {
        if byte.is_ascii_whitespace() {
            continue;
        }
        if byte == b'=' {
            padding += 1;
            continue;
        }
        if padding > 0 {
            return None;
        }
        symbols.push(value_of(byte)?);
    }
    if padding > 2 || (padding > 0 && !(symbols.len() + padding).is_multiple_of(4)) {
        return None;
    }
    if symbols.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(symbols.len() / 4 * 3 + 2);
    for group in symbols.chunks(4) {
        let mut block = 0u32;
        for (index, symbol) in group.iter().enumerate() {
            block |= symbol << (18 - 6 * index);
        }
        let [_, first, second, third] = block.to_be_bytes();
        let keep = group.len() - 1;
        out.extend_from_slice([first, second, third].get(..keep)?);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_the_rfc_4648_vectors() {
        for (plain, standard) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(encode(plain.as_bytes()), standard);
            assert_eq!(encode_url(plain.as_bytes()), standard.trim_end_matches('='));
            assert_eq!(decode(standard), Some(plain.as_bytes().to_vec()));
            assert_eq!(
                decode(standard.trim_end_matches('=')),
                Some(plain.as_bytes().to_vec())
            );
        }
    }

    #[test]
    fn the_url_alphabet_replaces_plus_and_slash() {
        let data = [0xfb, 0xff, 0xfe];
        assert_eq!(encode(&data), "+//+");
        assert_eq!(encode_url(&data), "-__-");
        assert_eq!(decode("+//+"), Some(data.to_vec()));
        assert_eq!(decode("-__-"), Some(data.to_vec()));
    }

    #[test]
    fn every_byte_value_round_trips() {
        let data: Vec<u8> = (0..=255).collect();
        assert_eq!(decode(&encode(&data)), Some(data.clone()));
        assert_eq!(decode(&encode_url(&data)), Some(data));
    }

    #[test]
    fn whitespace_is_skipped_and_bad_input_is_refused() {
        assert_eq!(decode("Zm9v\r\nYmFy"), Some(b"foobar".to_vec()));
        for bad in [
            "Z", "Zm9vY", "Zg=", "Zg===", "Zg=a", "Zm9v!", "=Zg", "Zm 9=v", "ä",
        ] {
            assert_eq!(decode(bad), None, "{bad:?}");
        }
    }
}
