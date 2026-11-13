use super::header;
use super::message::Message;
use crate::backend::Person;
use crate::texts::MAX_SUBJECT;

const MAX_REFERENCES: usize = 20;
const MAX_FROM_CHARS: usize = 200;
const PREFIX: &str = "Re:";

pub(crate) struct Original {
    thread_id: Option<String>,
    message_id: Option<String>,
    references: Vec<String>,
    subject: String,
    from_text: String,
    from: Vec<Person>,
    reply_to: Vec<Person>,
    to: Vec<Person>,
    cc: Vec<Person>,
}

pub(crate) struct Plan {
    pub(crate) to: Vec<String>,
    pub(crate) cc: Vec<String>,
    pub(crate) subject: String,
    pub(crate) in_reply_to: Option<String>,
    pub(crate) references: Vec<String>,
    pub(crate) thread_id: Option<String>,
    pub(crate) original_subject: String,
    pub(crate) original_from: String,
}

impl Original {
    pub(crate) fn of(message: &Message<'_>) -> Original {
        let people = |name: &str| message.header(name).map(header::people).unwrap_or_default();
        let ids = |name: &str| {
            message
                .header(name)
                .map(header::message_ids)
                .unwrap_or_default()
        };
        let mut references = ids("References");
        if references.is_empty() {
            references = ids("In-Reply-To");
        }
        Original {
            thread_id: message.thread_id().map(str::to_string),
            message_id: ids("Message-ID").into_iter().next(),
            references,
            subject: header::text(
                message.header("Subject").unwrap_or_default(),
                MAX_SUBJECT - PREFIX.len() - 1,
            ),
            from_text: header::text(message.header("From").unwrap_or_default(), MAX_FROM_CHARS),
            from: people("From"),
            reply_to: people("Reply-To"),
            to: people("To"),
            cc: people("Cc"),
        }
    }
}

fn add(list: &mut Vec<String>, address: &str) {
    if !list.iter().any(|known| known.eq_ignore_ascii_case(address)) {
        list.push(address.to_string());
    }
}

pub(crate) fn distinct(to: &[String], cc: &[String]) -> (Vec<String>, Vec<String>) {
    let mut first = Vec::new();
    for address in to {
        add(&mut first, address);
    }
    let mut second = Vec::new();
    for address in cc {
        if !first
            .iter()
            .any(|known| known.eq_ignore_ascii_case(address))
        {
            add(&mut second, address);
        }
    }
    (first, second)
}

fn unique(people: &[Person]) -> Vec<String> {
    let mut out = Vec::new();
    for person in people {
        add(&mut out, &person.address);
    }
    out
}

fn without(addresses: Vec<String>, own: &[String]) -> Vec<String> {
    addresses
        .into_iter()
        .filter(|address| !own.iter().any(|mine| mine.eq_ignore_ascii_case(address)))
        .collect()
}

fn reply_subject(subject: &str) -> String {
    let subject = subject.trim();
    if subject.is_empty() {
        return PREFIX.to_string();
    }
    let prefixed = subject
        .get(..PREFIX.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(PREFIX));
    if prefixed {
        subject.to_string()
    } else {
        format!("{PREFIX} {subject}")
    }
}

fn thread_references(original: &Original) -> Vec<String> {
    let mut references = original.references.clone();
    if let Some(id) = &original.message_id {
        references.retain(|known| known != id);
        references.push(id.clone());
    }
    let surplus = references.len().saturating_sub(MAX_REFERENCES);
    references.drain(..surplus);
    references
}

pub(crate) fn plan(original: &Original, own: &[String], all: bool) -> Plan {
    let target = if original.reply_to.is_empty() {
        unique(&original.from)
    } else {
        unique(&original.reply_to)
    };
    let mut to;
    let mut cc = Vec::new();
    if all {
        to = without(target, own);
        for address in without(unique(&original.to), own) {
            add(&mut to, &address);
        }
        for address in without(unique(&original.cc), own) {
            if !to.iter().any(|known| known.eq_ignore_ascii_case(&address)) {
                add(&mut cc, &address);
            }
        }
    } else {
        to = target.clone();
        if !to.is_empty() && without(to.clone(), own).is_empty() {
            let others = without(unique(&original.to), own);
            if !others.is_empty() {
                to = others;
            }
        }
    }
    Plan {
        to,
        cc,
        subject: reply_subject(&original.subject),
        in_reply_to: original.message_id.clone(),
        references: thread_references(original),
        thread_id: original.thread_id.clone(),
        original_subject: original.subject.clone(),
        original_from: original.from_text.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{j, TestResult};

    fn message(
        headers: &[(&str, &str)],
        extra: &str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let entries: Vec<String> = headers
            .iter()
            .map(|(name, value)| {
                format!(
                    "{{\"name\": {}, \"value\": {}}}",
                    crate::json::dumps(&crate::json::Json::from(*name), true),
                    crate::json::dumps(&crate::json::Json::from(*value), true)
                )
            })
            .collect();
        Ok(format!(
            "{{\"id\": \"m1\", \"threadId\": \"t100\"{extra}, \"payload\": {{\"headers\": [{}]}}}}",
            entries.join(", ")
        ))
    }

    fn parsed(headers: &[(&str, &str)]) -> Result<Original, Box<dyn std::error::Error>> {
        let json = j(&message(headers, "")?)?;
        let parsed = Message::parse(&json).ok_or("message")?;
        Ok(Original::of(&parsed))
    }

    fn own() -> Vec<String> {
        vec![
            "me@gmail.example".to_string(),
            "Alias@Gmail.Example".to_string(),
        ]
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn recipient_lists_are_made_distinct_without_regard_to_case() {
        let (to, cc) = distinct(
            &strings(&["a@example.org", "A@EXAMPLE.org", "b@example.org"]),
            &strings(&[
                "B@example.org",
                "c@example.org",
                "C@example.org",
                "a@example.org",
            ]),
        );
        assert_eq!(to, ["a@example.org", "b@example.org"]);
        assert_eq!(cc, ["c@example.org"]);
        assert_eq!(
            distinct(&[], &[]),
            (Vec::<String>::new(), Vec::<String>::new())
        );
    }

    #[test]
    fn a_reply_goes_to_the_sender() -> TestResult {
        let original = parsed(&[
            ("From", "Anna Muster <anna@example.org>"),
            ("To", "me@gmail.example, bob@example.org"),
            ("Cc", "carl@example.org"),
            ("Subject", "Plan"),
            ("Message-ID", "<m1@example.org>"),
        ])?;
        let plan = plan(&original, &own(), false);
        assert_eq!(plan.to, ["anna@example.org"]);
        assert!(plan.cc.is_empty());
        assert_eq!(plan.subject, "Re: Plan");
        assert_eq!(plan.in_reply_to.as_deref(), Some("<m1@example.org>"));
        assert_eq!(plan.references, ["<m1@example.org>"]);
        assert_eq!(plan.thread_id.as_deref(), Some("t100"));
        assert_eq!(plan.original_subject, "Plan");
        assert_eq!(plan.original_from, "Anna Muster <anna@example.org>");
        Ok(())
    }

    #[test]
    fn reply_to_wins_over_from() -> TestResult {
        let original = parsed(&[
            ("From", "Newsletter <news@example.org>"),
            ("Reply-To", "Support <help@example.org>, other@example.org"),
            ("Subject", "Offer"),
        ])?;
        assert_eq!(
            plan(&original, &own(), false).to,
            ["help@example.org", "other@example.org"]
        );
        let empty_reply_to = parsed(&[("From", "a@example.org"), ("Reply-To", "nonsense")])?;
        assert_eq!(plan(&empty_reply_to, &own(), false).to, ["a@example.org"]);
        Ok(())
    }

    #[test]
    fn reply_all_adds_to_and_cc_without_own_addresses_or_duplicates() -> TestResult {
        let original = parsed(&[
            ("From", "Anna <anna@example.org>"),
            ("To", "ME@gmail.example, Bob <bob@example.org>, ANNA@example.org"),
            ("Cc", "carl@example.org, alias@gmail.example, BOB@example.org, dora@example.org, carl@EXAMPLE.org"),
            ("Subject", "Plan"),
        ])?;
        let plan = plan(&original, &own(), true);
        assert_eq!(plan.to, ["anna@example.org", "bob@example.org"]);
        assert_eq!(plan.cc, ["carl@example.org", "dora@example.org"]);
        Ok(())
    }

    #[test]
    fn reply_all_with_reply_to_keeps_the_other_recipients() -> TestResult {
        let original = parsed(&[
            ("From", "Sender <s@example.org>"),
            ("Reply-To", "list@example.org"),
            ("To", "list@example.org, me@gmail.example"),
            ("Cc", "x@example.org"),
        ])?;
        let plan = plan(&original, &own(), true);
        assert_eq!(plan.to, ["list@example.org"]);
        assert_eq!(plan.cc, ["x@example.org"]);
        Ok(())
    }

    #[test]
    fn answering_an_own_message_goes_to_its_recipients() -> TestResult {
        let original = parsed(&[
            ("From", "Me <me@gmail.example>"),
            ("To", "Anna <anna@example.org>, bob@example.org"),
            ("Cc", "carl@example.org"),
            ("Subject", "Plan"),
        ])?;
        assert_eq!(
            plan(&original, &own(), false).to,
            ["anna@example.org", "bob@example.org"]
        );
        let all = plan(&original, &own(), true);
        assert_eq!(all.to, ["anna@example.org", "bob@example.org"]);
        assert_eq!(all.cc, ["carl@example.org"]);
        let to_myself = parsed(&[("From", "me@gmail.example"), ("To", "me@gmail.example")])?;
        assert_eq!(plan(&to_myself, &own(), false).to, ["me@gmail.example"]);
        assert!(plan(&to_myself, &own(), true).to.is_empty());
        Ok(())
    }

    #[test]
    fn a_reply_with_several_senders_keeps_all_of_them() -> TestResult {
        let original = parsed(&[("From", "a@example.org, me@gmail.example")])?;
        assert_eq!(
            plan(&original, &own(), false).to,
            ["a@example.org", "me@gmail.example"]
        );
        assert_eq!(plan(&original, &own(), true).to, ["a@example.org"]);
        Ok(())
    }

    #[test]
    fn nothing_to_reply_to_gives_empty_lists() -> TestResult {
        let original = parsed(&[("Subject", "x")])?;
        let plan = plan(&original, &own(), true);
        assert!(plan.to.is_empty() && plan.cc.is_empty());
        let bare = parsed(&[("From", "undisclosed-recipients:;")])?;
        assert!(super::plan(&bare, &own(), false).to.is_empty());
        Ok(())
    }

    #[test]
    fn the_subject_gets_one_prefix() {
        for (subject, wanted) in [
            ("Plan", "Re: Plan"),
            ("Re: Plan", "Re: Plan"),
            ("RE: Plan", "RE: Plan"),
            ("re: plan", "re: plan"),
            ("Re:Plan", "Re:Plan"),
            ("  Plan  ", "Re: Plan"),
            ("Reply: Plan", "Re: Reply: Plan"),
            ("Resume", "Re: Resume"),
            ("AW: Plan", "Re: AW: Plan"),
            ("", "Re:"),
            ("   ", "Re:"),
            ("R", "Re: R"),
            ("\u{e4}\u{e4}", "Re: \u{e4}\u{e4}"),
        ] {
            assert_eq!(reply_subject(subject), wanted, "{subject:?}");
        }
    }

    #[test]
    fn encoded_and_hostile_subjects_are_decoded_and_made_one_line() -> TestResult {
        let encoded = parsed(&[("Subject", "=?UTF-8?Q?Gr=C3=BC=C3=9Fe?=")])?;
        assert_eq!(plan(&encoded, &own(), false).subject, "Re: Gr\u{fc}\u{df}e");
        let hostile = parsed(&[("Subject", "Plan\r\nBcc: x@example.org\u{202e}")])?;
        assert_eq!(
            plan(&hostile, &own(), false).subject,
            "Re: Plan Bcc: x@example.org"
        );
        let long = parsed(&[("Subject", &"s".repeat(1000))])?;
        let plan = plan(&long, &own(), false);
        assert_eq!(plan.subject.chars().count(), 4 + (MAX_SUBJECT - 4) + 6);
        assert!(plan.subject.ends_with(" [...]"));
        Ok(())
    }

    #[test]
    fn threading_headers_extend_the_chain() -> TestResult {
        let original = parsed(&[
            ("Message-ID", "<m3@x.example>"),
            ("References", "<m1@x.example>\r\n <m2@x.example>"),
            ("In-Reply-To", "<m2@x.example>"),
        ])?;
        let plan = plan(&original, &own(), false);
        assert_eq!(plan.in_reply_to.as_deref(), Some("<m3@x.example>"));
        assert_eq!(
            plan.references,
            ["<m1@x.example>", "<m2@x.example>", "<m3@x.example>"]
        );
        let again = parsed(&[
            ("Message-ID", "<m3@x.example>"),
            ("References", "<m3@x.example> <m4@x.example>"),
        ])?;
        assert_eq!(plan_refs(&again), ["<m4@x.example>", "<m3@x.example>"]);
        Ok(())
    }

    fn plan_refs(original: &Original) -> Vec<String> {
        plan(original, &own(), false).references
    }

    #[test]
    fn in_reply_to_stands_in_for_missing_references() -> TestResult {
        let original = parsed(&[
            ("Message-ID", "<m2@x.example>"),
            ("In-Reply-To", "<m1@x.example>"),
        ])?;
        assert_eq!(plan_refs(&original), ["<m1@x.example>", "<m2@x.example>"]);
        Ok(())
    }

    #[test]
    fn without_a_message_id_there_is_no_parent_but_the_chain_is_kept() -> TestResult {
        let original = parsed(&[("References", "<m1@x.example>")])?;
        let plan = plan(&original, &own(), false);
        assert_eq!(plan.in_reply_to, None);
        assert_eq!(plan.references, ["<m1@x.example>"]);
        let none = parsed(&[("Subject", "x")])?;
        let plan = super::plan(&none, &own(), false);
        assert_eq!(plan.in_reply_to, None);
        assert!(plan.references.is_empty());
        Ok(())
    }

    #[test]
    fn hostile_message_ids_are_dropped() -> TestResult {
        let original = parsed(&[
            ("Message-ID", "<bad id@x.example>"),
            (
                "References",
                "<ok@x.example> <a\r\nBcc: x@example.org> <\u{e4}@x.example>",
            ),
        ])?;
        let plan = plan(&original, &own(), false);
        assert_eq!(plan.in_reply_to, None);
        assert_eq!(plan.references, ["<ok@x.example>"]);
        Ok(())
    }

    #[test]
    fn the_chain_keeps_only_the_most_recent_references() -> TestResult {
        let refs: Vec<String> = (0..40).map(|n| format!("<r{n}@x.example>")).collect();
        let original = parsed(&[
            ("Message-ID", "<last@x.example>"),
            ("References", &refs.join(" ")),
        ])?;
        let references = plan_refs(&original);
        assert_eq!(references.len(), MAX_REFERENCES);
        assert_eq!(
            references.last().map(String::as_str),
            Some("<last@x.example>")
        );
        assert_eq!(
            references.first().map(String::as_str),
            Some("<r21@x.example>")
        );
        Ok(())
    }

    #[test]
    fn the_thread_id_must_be_a_plain_token() -> TestResult {
        let json = j(&message(&[("From", "a@example.org")], "")?.replace("t100", "bad id"))?;
        let parsed = Message::parse(&json).ok_or("message")?;
        assert_eq!(plan(&Original::of(&parsed), &own(), false).thread_id, None);
        Ok(())
    }

    #[test]
    fn address_lists_are_bounded() -> TestResult {
        let many: Vec<String> = (0..500).map(|n| format!("u{n}@example.org")).collect();
        let original = parsed(&[
            ("From", "a@example.org"),
            ("To", &many.join(", ")),
            ("Cc", &many.join(", ")),
        ])?;
        let plan = plan(&original, &own(), true);
        assert_eq!(plan.to.len(), 1 + 100);
        assert!(plan.cc.is_empty());
        Ok(())
    }
}
