use crate::json::{Json, Object};

const MAX_HEADERS: usize = 500;
const MAX_LABELS: usize = 100;
const MAX_THREAD_ID: usize = 64;
const MAX_MILLIS: i64 = 253_402_300_799_000;

pub(crate) fn whole(value: Option<&Json>) -> Option<i64> {
    match value? {
        Json::Int(number) => Some(*number),
        Json::Str(text) => text.trim().parse::<i64>().ok(),
        _ => None,
    }
}

pub(crate) fn header<'a>(headers: Option<&'a Json>, name: &str) -> Option<&'a str> {
    headers?
        .as_array()?
        .iter()
        .take(MAX_HEADERS)
        .filter_map(Json::as_object)
        .find(|entry| {
            entry
                .get("name")
                .and_then(Json::as_str)
                .is_some_and(|found| found.eq_ignore_ascii_case(name))
        })?
        .get("value")?
        .as_str()
}

#[derive(Clone, Copy)]
pub(crate) struct Message<'a> {
    object: &'a Object,
}

impl<'a> Message<'a> {
    pub(crate) fn parse(json: &'a Json) -> Option<Message<'a>> {
        json.as_object().map(|object| Message { object })
    }

    pub(crate) fn thread_id(&self) -> Option<&'a str> {
        self.object
            .get("threadId")
            .and_then(Json::as_str)
            .filter(|id| {
                !id.is_empty()
                    && id.len() <= MAX_THREAD_ID
                    && id
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            })
    }

    pub(crate) fn labels(&self) -> impl Iterator<Item = &'a str> {
        self.object
            .get("labelIds")
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
            .filter_map(Json::as_str)
            .take(MAX_LABELS)
    }

    pub(crate) fn has_label(&self, label: &str) -> bool {
        self.labels().any(|found| found == label)
    }

    pub(crate) fn seconds(&self) -> Option<i64> {
        whole(self.object.get("internalDate"))
            .filter(|millis| (0..=MAX_MILLIS).contains(millis))
            .map(|millis| millis / 1000)
    }

    pub(crate) fn payload(&self) -> Option<&'a Json> {
        self.object.get("payload")
    }

    pub(crate) fn header(&self, name: &str) -> Option<&'a str> {
        header(
            self.payload()
                .and_then(Json::as_object)
                .and_then(|payload| payload.get("headers")),
            name,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{j, TestResult};

    #[test]
    fn headers_are_found_by_name_without_regard_to_case() -> TestResult {
        let headers = j(
            r#"[{"name": "From", "value": "a@x.example"}, {"name": "SUBJECT", "value": "S1"},
                {"name": "subject", "value": "S2"}, {"name": "X-Empty", "value": ""},
                {"name": "X-Number", "value": 5}, {"value": "no name"}, 7, null]"#,
        )?;
        assert_eq!(header(Some(&headers), "from"), Some("a@x.example"));
        assert_eq!(header(Some(&headers), "Subject"), Some("S1"));
        assert_eq!(header(Some(&headers), "x-empty"), Some(""));
        assert_eq!(header(Some(&headers), "x-number"), None);
        assert_eq!(header(Some(&headers), "to"), None);
        assert_eq!(header(Some(&j("{}")?), "from"), None);
        assert_eq!(header(Some(&j("[]")?), "from"), None);
        assert_eq!(header(None, "from"), None);
        Ok(())
    }

    #[test]
    fn only_a_bounded_number_of_headers_is_searched() -> TestResult {
        let mut entries = vec!["{\"name\": \"X\", \"value\": \"v\"}"; MAX_HEADERS];
        entries.push("{\"name\": \"Late\", \"value\": \"found\"}");
        let headers = j(&format!("[{}]", entries.join(",")))?;
        assert_eq!(header(Some(&headers), "x"), Some("v"));
        assert_eq!(header(Some(&headers), "late"), None);
        Ok(())
    }

    #[test]
    fn whole_numbers_come_from_numbers_or_numeric_text() -> TestResult {
        assert_eq!(whole(Some(&j("42")?)), Some(42));
        assert_eq!(whole(Some(&j("\" 42 \"")?)), Some(42));
        assert_eq!(whole(Some(&j("-3")?)), Some(-3));
        for bad in ["\"x\"", "1.5", "true", "null", "[]", "{}", "\"\""] {
            assert_eq!(whole(Some(&j(bad)?)), None, "{bad}");
        }
        assert_eq!(whole(None), None);
        Ok(())
    }

    #[test]
    fn message_fields_are_read_defensively() -> TestResult {
        let json = j(
            r#"{"id": "m1", "threadId": "18c3f0a1b2", "labelIds": ["INBOX", 5, "UNREAD", null],
                "internalDate": "1700000000999",
                "payload": {"headers": [{"name": "Subject", "value": "Hallo"}]}}"#,
        )?;
        let message = Message::parse(&json).ok_or("message")?;
        assert_eq!(message.thread_id(), Some("18c3f0a1b2"));
        assert_eq!(message.labels().collect::<Vec<_>>(), ["INBOX", "UNREAD"]);
        assert!(message.has_label("UNREAD"));
        assert!(!message.has_label("STARRED"));
        assert_eq!(message.seconds(), Some(1_700_000_000));
        assert_eq!(message.header("subject"), Some("Hallo"));
        assert_eq!(message.header("from"), None);
        assert!(message.payload().is_some());
        let empty = j("{}")?;
        let message = Message::parse(&empty).ok_or("message")?;
        assert_eq!(message.thread_id(), None);
        assert_eq!(message.labels().count(), 0);
        assert_eq!(message.seconds(), None);
        assert_eq!(message.header("subject"), None);
        assert!(Message::parse(&j("[]")?).is_none());
        assert!(Message::parse(&j("\"m\"")?).is_none());
        Ok(())
    }

    #[test]
    fn the_internal_date_must_be_a_plausible_instant() -> TestResult {
        for (date, wanted) in [
            ("\"0\"", Some(0)),
            ("1700000000000", Some(1_700_000_000)),
            ("\"253402300799000\"", Some(253_402_300_799)),
            ("\"253402300800000\"", None),
            ("\"-1\"", None),
            ("\"abc\"", None),
            ("\"\"", None),
            ("1.5", None),
            ("true", None),
            ("null", None),
            ("99999999999999999999999", None),
        ] {
            let json = j(&format!("{{\"internalDate\": {date}}}"))?;
            let message = Message::parse(&json).ok_or("message")?;
            assert_eq!(message.seconds(), wanted, "{date}");
        }
        Ok(())
    }

    #[test]
    fn thread_ids_are_plain_tokens() -> TestResult {
        for (id, wanted) in [
            ("\"18c3f0a1b2c3d4e5\"", true),
            ("\"a_b-C9\"", true),
            ("\"\"", false),
            ("\"a b\"", false),
            ("\"a/b\"", false),
            ("\"a\\nb\"", false),
            ("\"\u{e4}\"", false),
            ("5", false),
        ] {
            let json = j(&format!("{{\"threadId\": {id}}}"))?;
            let message = Message::parse(&json).ok_or("message")?;
            assert_eq!(message.thread_id().is_some(), wanted, "{id}");
        }
        let long = j(&format!(
            "{{\"threadId\": \"{}\"}}",
            "a".repeat(MAX_THREAD_ID + 1)
        ))?;
        assert_eq!(Message::parse(&long).ok_or("message")?.thread_id(), None);
        Ok(())
    }

    #[test]
    fn labels_are_bounded() -> TestResult {
        let labels: Vec<String> = (0..MAX_LABELS + 20).map(|n| format!("\"L{n}\"")).collect();
        let json = j(&format!("{{\"labelIds\": [{}]}}", labels.join(",")))?;
        let message = Message::parse(&json).ok_or("message")?;
        assert_eq!(message.labels().count(), MAX_LABELS);
        assert!(message.has_label("L0"));
        assert!(!message.has_label(&format!("L{}", MAX_LABELS + 1)));
        Ok(())
    }
}
