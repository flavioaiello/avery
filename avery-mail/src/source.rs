#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    Apple,
    Google,
    Microsoft,
}

impl Source {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Source::Apple => "apple",
            Source::Google => "google",
            Source::Microsoft => "microsoft",
        }
    }

    pub(crate) fn parse_cloud(text: &str) -> Option<Source> {
        match text {
            "google" => Some(Source::Google),
            "microsoft" => Some(Source::Microsoft),
            _ => None,
        }
    }

    pub(crate) fn kind(self) -> &'static str {
        match self {
            Source::Apple => "apple",
            Source::Google => "gmail",
            Source::Microsoft => "outlook",
        }
    }

    pub(crate) fn of_id(id: &str) -> Source {
        match id.split_once(':') {
            Some(("google", _)) => Source::Google,
            Some(("microsoft", _)) => Source::Microsoft,
            _ => Source::Apple,
        }
    }
}

pub(crate) const MAX_ADDRESS: usize = 254;
pub(crate) const MAX_CALENDAR_ID: usize = 512;

pub(crate) fn plain_address(text: &str) -> bool {
    let Some((local, domain)) = text.split_once('@') else {
        return false;
    };
    text.len() <= MAX_ADDRESS
        && !local.is_empty()
        && local.len() <= 64
        && domain.contains('.')
        && !domain.contains('@')
        && local
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._%+-".contains(c))
        && domain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".-".contains(c))
        && !domain.starts_with(['.', '-'])
        && !domain.ends_with(['.', '-'])
        && !domain.contains("..")
}

pub(crate) fn account_id(source: Source, address: &str) -> Option<String> {
    (source != Source::Apple && plain_address(address))
        .then(|| format!("{}:{address}", source.as_str()))
}

pub(crate) fn account_address(id: &str) -> Option<(Source, &str)> {
    let source = Source::of_id(id);
    let (_, address) = id.split_once(':')?;
    (source != Source::Apple && plain_address(address)).then_some((source, address))
}

fn calendar_token(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_CALENDAR_ID
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._%+-=@#~".contains(c))
}

pub(crate) fn calendar_id(source: Source, address: &str, calendar: &str) -> Option<String> {
    (source != Source::Apple && plain_address(address) && calendar_token(calendar))
        .then(|| format!("{}:{address}:{calendar}", source.as_str()))
}

pub(crate) fn calendar_parts(id: &str) -> Option<(Source, &str, &str)> {
    let source = Source::of_id(id);
    if source == Source::Apple {
        return None;
    }
    let mut parts = id.splitn(3, ':');
    parts.next()?;
    let address = parts.next()?;
    let calendar = parts.next()?;
    (plain_address(address) && calendar_token(calendar)).then_some((source, address, calendar))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_source_follows_the_id_prefix() {
        assert_eq!(Source::of_id("google:a@gmail.example"), Source::Google);
        assert_eq!(
            Source::of_id("microsoft:a@outlook.example"),
            Source::Microsoft
        );
        assert_eq!(Source::of_id("5F3E-4C"), Source::Apple);
        assert_eq!(Source::of_id("googl:x"), Source::Apple);
        assert_eq!(Source::of_id("Google:x"), Source::Apple);
        assert_eq!(Source::of_id(""), Source::Apple);
        assert_eq!(Source::parse_cloud("google"), Some(Source::Google));
        assert_eq!(Source::parse_cloud("apple"), None);
        assert_eq!(Source::Google.kind(), "gmail");
        assert_eq!(Source::Microsoft.kind(), "outlook");
    }

    #[test]
    fn account_ids_round_trip_and_refuse_odd_addresses() {
        let id = account_id(Source::Google, "alice@gmail.example");
        assert_eq!(id.as_deref(), Some("google:alice@gmail.example"));
        assert_eq!(
            account_address("google:alice@gmail.example"),
            Some((Source::Google, "alice@gmail.example"))
        );
        assert_eq!(account_id(Source::Apple, "alice@gmail.example"), None);
        for bad in [
            "",
            "alice",
            "alice@",
            "@x.example",
            "a b@x.example",
            "a:b@x.example",
            "a@x",
            "a@x..example",
            "a@-x.example",
            "a@x.example-",
            "a@b@x.example",
            "ä@x.example",
            "a\"@x.example",
        ] {
            assert_eq!(account_id(Source::Google, bad), None, "{bad:?}");
        }
        assert_eq!(account_address("5F3E-4C"), None);
        assert_eq!(account_address("google:nonsense"), None);
    }

    #[test]
    fn calendar_ids_keep_the_account_and_the_provider_calendar_apart() {
        let id = calendar_id(
            Source::Google,
            "alice@gmail.example",
            "family123@group.calendar.google.example",
        );
        assert_eq!(
            id.as_deref(),
            Some("google:alice@gmail.example:family123@group.calendar.google.example")
        );
        assert_eq!(
            calendar_parts("google:alice@gmail.example:family123@group.calendar.google.example"),
            Some((
                Source::Google,
                "alice@gmail.example",
                "family123@group.calendar.google.example"
            ))
        );
        assert_eq!(
            calendar_parts("microsoft:b@outlook.example:AQMkAD-_=="),
            Some((Source::Microsoft, "b@outlook.example", "AQMkAD-_=="))
        );
        assert_eq!(calendar_parts("ABC-123"), None);
        assert_eq!(calendar_parts("google:a@gmail.example"), None);
        assert_eq!(calendar_parts("google:a@gmail.example:"), None);
        assert_eq!(calendar_parts("google:a@gmail.example:x y"), None);
        assert_eq!(calendar_id(Source::Google, "a@gmail.example", "x/y"), None);
        assert_eq!(calendar_id(Source::Google, "a@gmail.example", ""), None);
        let long = "x".repeat(MAX_CALENDAR_ID + 1);
        assert_eq!(calendar_id(Source::Google, "a@gmail.example", &long), None);
    }
}
