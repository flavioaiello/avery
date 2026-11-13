use super::{
    field, next_link, page_items, url, MAX_ID, MAX_NAME, NO_MAILBOX, PAGE_ITEMS, PAGE_SIZE,
};
use crate::backend::Mailbox;
use crate::error::{ToolError, ToolResult};
use crate::http::encode_component;
use crate::json::Json;
use crate::provider::Api;
use crate::text::one_line_str;

const MAX_FOLDERS: usize = 150;
const LEVELS: usize = 2;
const PAGES: usize = 2;
const FANOUT: usize = 50;
const FIELDS: &str = "id,displayName,unreadItemCount,childFolderCount";
const WELL_KNOWN: [(&str, &str, &[&str]); 6] = [
    ("inbox", "Inbox", &["inbox"]),
    (
        "sentitems",
        "Sent Items",
        &["sent", "sent items", "sentitems"],
    ),
    ("drafts", "Drafts", &["drafts"]),
    (
        "deleteditems",
        "Deleted Items",
        &["deleted items", "deleteditems", "trash"],
    ),
    (
        "junkemail",
        "Junk Email",
        &["junk", "junk email", "junkemail", "spam"],
    ),
    ("archive", "Archive", &["archive"]),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Place {
    pub(crate) id: String,
    pub(crate) name: String,
}

struct Entry {
    id: String,
    name: String,
    path: String,
    unread: String,
    nested: bool,
    order: Vec<usize>,
}

fn normalize(text: &str) -> String {
    text.split('/')
        .map(|part| part.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|part| !part.is_empty())
        .map(|part| part.to_lowercase())
        .collect::<Vec<_>>()
        .join("/")
}

fn entries_of(json: &Json, parent: Option<&Entry>, offset: usize) -> ToolResult<Vec<Entry>> {
    let mut found = Vec::new();
    for (index, item) in page_items(json)?.iter().take(PAGE_ITEMS).enumerate() {
        let Some(folder) = item.as_object() else {
            continue;
        };
        let id = field(folder, "id");
        let name = one_line_str(field(folder, "displayName"), MAX_NAME);
        if id.is_empty() || id.len() > MAX_ID || name.is_empty() {
            continue;
        }
        let (path, mut order) = match parent {
            Some(owner) => (format!("{}/{name}", owner.path), owner.order.clone()),
            None => (name.clone(), Vec::new()),
        };
        order.push(offset + index);
        found.push(Entry {
            id: id.to_string(),
            path,
            name,
            unread: match folder.get("unreadItemCount") {
                Some(Json::Int(count)) if *count >= 0 => count.to_string(),
                _ => "0".to_string(),
            },
            nested: matches!(folder.get("childFolderCount"), Some(Json::Int(count)) if *count > 0),
            order,
        });
    }
    Ok(found)
}

fn children_url(id: &str) -> String {
    url(
        &format!("/mailFolders/{}/childFolders", encode_component(id)),
        &[("$top", PAGE_SIZE), ("$select", FIELDS)],
    )
}

fn top_level(api: &Api) -> ToolResult<Vec<Entry>> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut next = Some(url(
        "/mailFolders",
        &[("$top", PAGE_SIZE), ("$select", FIELDS)],
    ));
    for _ in 0..PAGES {
        let Some(address) = next.take() else {
            break;
        };
        let json = api.get(&address)?;
        let room = MAX_FOLDERS.saturating_sub(entries.len());
        let page = entries_of(&json, None, entries.len())?;
        entries.extend(page.into_iter().take(room));
        if entries.len() < MAX_FOLDERS {
            next = next_link(&json)?;
        }
    }
    Ok(entries)
}

fn descend(api: &Api, entries: &mut Vec<Entry>) -> ToolResult<()> {
    let mut frontier: Vec<usize> = (0..entries.len()).collect();
    for _ in 0..LEVELS {
        let room = MAX_FOLDERS.saturating_sub(entries.len());
        let jobs: Vec<(usize, String)> = frontier
            .iter()
            .filter_map(|index| {
                let entry = entries.get(*index)?;
                entry.nested.then(|| (*index, children_url(&entry.id)))
            })
            .take(FANOUT.min(room))
            .collect();
        if jobs.is_empty() {
            break;
        }
        let addresses: Vec<String> = jobs.iter().map(|(_, address)| address.clone()).collect();
        frontier = Vec::new();
        for ((parent, _), answer) in jobs.iter().zip(api.get_many(&addresses)) {
            let json = answer?;
            let children = match entries.get(*parent) {
                Some(owner) => entries_of(&json, Some(owner), 0)?,
                None => continue,
            };
            let room = MAX_FOLDERS.saturating_sub(entries.len());
            for child in children.into_iter().take(room) {
                frontier.push(entries.len());
                entries.push(child);
            }
        }
    }
    Ok(())
}

fn tree(api: &Api) -> ToolResult<Vec<Entry>> {
    let mut entries = top_level(api)?;
    descend(api, &mut entries)?;
    Ok(entries)
}

pub(crate) fn listing(api: &Api) -> ToolResult<Vec<Mailbox>> {
    let mut entries = tree(api)?;
    entries.sort_by(|left, right| left.order.cmp(&right.order));
    Ok(entries
        .into_iter()
        .map(|entry| Mailbox {
            path: entry.path,
            unread: entry.unread,
        })
        .collect())
}

fn place((id, name, _): &(&str, &str, &[&str])) -> Place {
    Place {
        id: (*id).to_string(),
        name: (*name).to_string(),
    }
}

pub(crate) fn resolve(api: &Api, wanted: Option<&str>) -> ToolResult<Place> {
    let text = wanted.unwrap_or_default().trim();
    let wanted = normalize(text);
    let known = if text.is_empty() {
        WELL_KNOWN.first()
    } else {
        WELL_KNOWN
            .iter()
            .find(|(_, _, aliases)| aliases.contains(&wanted.as_str()))
    };
    if let Some(entry) = known {
        return Ok(place(entry));
    }
    if wanted.is_empty() {
        return Err(ToolError::msg(NO_MAILBOX));
    }
    let entries = tree(api)?;
    entries
        .iter()
        .find(|entry| normalize(&entry.path) == wanted)
        .or_else(|| {
            entries
                .iter()
                .find(|entry| normalize(&entry.name) == wanted)
        })
        .map(|entry| Place {
            id: entry.id.clone(),
            name: entry.path.clone(),
        })
        .ok_or_else(|| ToolError::msg(NO_MAILBOX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::scripted::Scripted;
    use crate::http::{HttpError, Method};
    use crate::provider::Provider;
    use crate::testutil::TestResult;

    const TOP_URL: &str = concat!(
        "https://graph.microsoft.com/v1.0/me/mailFolders",
        "?$top=100&$select=id%2CdisplayName%2CunreadItemCount%2CchildFolderCount"
    );
    const SELECT: &str = "?$top=100&$select=id%2CdisplayName%2CunreadItemCount%2CchildFolderCount";

    fn api(transport: &Scripted) -> Api<'_> {
        Api::new(transport, "TOKEN".to_string(), Provider::Microsoft)
    }

    fn folder(id: &str, name: &str, unread: i64, children: i64) -> String {
        format!(
            r#"{{"id": "{id}", "displayName": "{name}", "unreadItemCount": {unread}, "childFolderCount": {children}}}"#
        )
    }

    fn folders(entries: &[String], next: Option<&str>) -> String {
        let link = next.map_or_else(String::new, |link| {
            format!(r#""@odata.nextLink": "{link}", "#)
        });
        format!(
            r#"{{"@odata.context": "https://graph.microsoft.com/v1.0/$metadata#users('bb8775a4')/mailFolders", {link}"value": [{}]}}"#,
            entries.join(", ")
        )
    }

    fn mailbox() -> Scripted {
        Scripted::new()
            .json(
                Method::Get,
                "/mailFolders/AAMkProjects%3D/childFolders",
                200,
                &folders(&[folder("AAMk2026=", "2026", 4, 1)], None),
            )
            .json(
                Method::Get,
                "/mailFolders/AAMkInbox%3D/childFolders",
                200,
                &folders(
                    &[
                        folder("AAMkProjects=", "Projects", 1, 1),
                        folder("AAMkReceipts=", "Receipts", 0, 0),
                    ],
                    None,
                ),
            )
            .json(
                Method::Get,
                "/mailFolders?",
                200,
                &folders(
                    &[
                        folder("AAMkInbox=", "Inbox", 3, 2),
                        folder("AAMkDrafts=", "Drafts", 0, 0),
                        folder("AAMkSent=", "Sent Items", 0, 0),
                        folder("AAMkArchive=", "Archive", 7, 0),
                    ],
                    None,
                ),
            )
    }

    fn paths(boxes: &[Mailbox]) -> Vec<(&str, &str)> {
        boxes
            .iter()
            .map(|entry| (entry.path.as_str(), entry.unread.as_str()))
            .collect()
    }

    #[test]
    fn the_tree_is_listed_in_reading_order_with_paths_and_unread_counts() -> TestResult {
        let transport = mailbox();
        let boxes = listing(&api(&transport))?;
        assert_eq!(
            paths(&boxes),
            [
                ("Inbox", "3"),
                ("Inbox/Projects", "1"),
                ("Inbox/Projects/2026", "4"),
                ("Inbox/Receipts", "0"),
                ("Drafts", "0"),
                ("Sent Items", "0"),
                ("Archive", "7"),
            ]
        );
        Ok(())
    }

    #[test]
    fn folders_are_requested_level_by_level_and_never_deeper_than_two_levels() -> TestResult {
        let transport = mailbox();
        listing(&api(&transport))?;
        let seen = transport.seen();
        let mut urls: Vec<String> = seen.iter().map(|call| call.url.clone()).collect();
        urls.sort();
        let mut wanted = vec![
            TOP_URL.to_string(),
            format!(
                "https://graph.microsoft.com/v1.0/me/mailFolders/AAMkInbox%3D/childFolders{SELECT}"
            ),
            format!(
                "https://graph.microsoft.com/v1.0/me/mailFolders/AAMkProjects%3D/childFolders{SELECT}"
            ),
        ];
        wanted.sort();
        assert_eq!(urls, wanted);
        assert_eq!(seen.first().map(|call| call.url.as_str()), Some(TOP_URL));
        for call in &seen {
            assert_eq!(call.method, Method::Get);
            assert_eq!(call.body, None);
            assert!(call.headers.iter().any(|(name, _)| name == "Authorization"));
        }
        Ok(())
    }

    #[test]
    fn entries_without_id_or_name_are_skipped_and_text_is_cleaned() -> TestResult {
        let transport = Scripted::new().json(
            Method::Get,
            "/mailFolders?",
            200,
            r#"{"value": [
                {"id": "A=", "displayName": "Plain"},
                {"id": "B=", "displayName": "Line\nBreak\u202e", "unreadItemCount": -3, "childFolderCount": 0},
                {"id": "", "displayName": "No id"},
                {"displayName": "No id either"},
                {"id": "C="},
                {"id": "D=", "displayName": "   "},
                {"id": "E=", "displayName": 5},
                {"id": "F=", "displayName": "Odd", "unreadItemCount": "7", "childFolderCount": "2"},
                "junk",
                null,
                []
            ]}"#,
        );
        let boxes = listing(&api(&transport))?;
        assert_eq!(
            paths(&boxes),
            [("Plain", "0"), ("Line Break", "0"), ("Odd", "0")]
        );
        assert_eq!(transport.seen().len(), 1);
        Ok(())
    }

    #[test]
    fn long_names_and_ids_are_bounded() -> TestResult {
        let long_name = "n".repeat(500);
        let long_id = "i".repeat(MAX_ID + 1);
        let transport = Scripted::new().json(
            Method::Get,
            "/mailFolders?",
            200,
            &folders(
                &[
                    folder("A=", &long_name, 0, 0),
                    folder(&long_id, "Skipped", 0, 0),
                ],
                None,
            ),
        );
        let boxes = listing(&api(&transport))?;
        let [only] = boxes.as_slice() else {
            return Err("one folder".into());
        };
        assert_eq!(only.path.chars().count(), MAX_NAME + 6);
        assert!(only.path.ends_with(" [...]"));
        Ok(())
    }

    #[test]
    fn at_most_one_hundred_fifty_folders_are_listed() -> TestResult {
        let first: Vec<String> = (0..100)
            .map(|number| folder(&format!("F{number:03}="), &format!("F{number:03}"), 0, 0))
            .collect();
        let second: Vec<String> = (100..200)
            .map(|number| folder(&format!("F{number:03}="), &format!("F{number:03}"), 0, 0))
            .collect();
        let link = "https://graph.microsoft.com/v1.0/me/mailFolders?%24skiptoken=P2";
        let transport = Scripted::new()
            .json(
                Method::Get,
                "skiptoken=P2",
                200,
                &folders(
                    &second,
                    Some("https://graph.microsoft.com/v1.0/me/mailFolders?%24skiptoken=P3"),
                ),
            )
            .json(
                Method::Get,
                "/mailFolders?",
                200,
                &folders(&first, Some(link)),
            );
        let boxes = listing(&api(&transport))?;
        assert_eq!(boxes.len(), MAX_FOLDERS);
        assert_eq!(boxes.first().map(|entry| entry.path.as_str()), Some("F000"));
        assert_eq!(boxes.last().map(|entry| entry.path.as_str()), Some("F149"));
        assert_eq!(transport.seen().len(), 2);
        Ok(())
    }

    #[test]
    fn a_second_page_of_top_level_folders_is_followed_but_not_a_third() -> TestResult {
        let page = |from: usize, next: Option<&str>| {
            let entries: Vec<String> = (from..from + 10)
                .map(|number| folder(&format!("F{number:03}="), &format!("F{number:03}"), 0, 0))
                .collect();
            folders(&entries, next)
        };
        let base = "https://graph.microsoft.com/v1.0/me/mailFolders?%24skiptoken=";
        let transport = Scripted::new()
            .json(Method::Get, "skiptoken=P3", 200, &page(20, None))
            .json(
                Method::Get,
                "skiptoken=P2",
                200,
                &page(10, Some(&format!("{base}P3"))),
            )
            .json(
                Method::Get,
                "/mailFolders?",
                200,
                &page(0, Some(&format!("{base}P2"))),
            );
        let boxes = listing(&api(&transport))?;
        assert_eq!(boxes.len(), 20);
        assert_eq!(transport.seen().len(), PAGES);
        Ok(())
    }

    #[test]
    fn children_use_the_room_the_top_level_leaves_and_requests_stay_bounded() -> TestResult {
        let top: Vec<String> = (0..100)
            .map(|number| folder(&format!("F{number:03}="), &format!("F{number:03}"), 0, 1))
            .collect();
        let transport = Scripted::new()
            .json(
                Method::Get,
                "/childFolders",
                200,
                &folders(&[folder("K=", "Kid", 2, 1)], None),
            )
            .json(Method::Get, "/mailFolders?", 200, &folders(&top, None));
        let boxes = listing(&api(&transport))?;
        assert_eq!(boxes.len(), MAX_FOLDERS);
        assert_eq!(transport.seen().len(), 1 + FANOUT);
        let kids = boxes
            .iter()
            .filter(|entry| entry.path.ends_with("/Kid"))
            .count();
        assert_eq!(kids, FANOUT);
        let order: Vec<&str> = boxes
            .iter()
            .take(4)
            .map(|entry| entry.path.as_str())
            .collect();
        assert_eq!(order, ["F000", "F000/Kid", "F001", "F001/Kid"]);
        Ok(())
    }

    #[test]
    fn a_full_tree_asks_for_no_children_at_all() -> TestResult {
        let page = |from: usize, count: usize, next: Option<&str>| {
            let entries: Vec<String> = (from..from + count)
                .map(|number| folder(&format!("F{number:03}="), &format!("F{number:03}"), 0, 1))
                .collect();
            folders(&entries, next)
        };
        let link = "https://graph.microsoft.com/v1.0/me/mailFolders?%24skiptoken=P2";
        let transport = Scripted::new()
            .json(Method::Get, "skiptoken=P2", 200, &page(100, 50, None))
            .json(Method::Get, "/mailFolders?", 200, &page(0, 100, Some(link)));
        let boxes = listing(&api(&transport))?;
        assert_eq!(boxes.len(), MAX_FOLDERS);
        assert_eq!(transport.seen().len(), 2);
        Ok(())
    }

    #[test]
    fn only_the_first_hundred_entries_of_a_folder_page_are_looked_at() -> TestResult {
        let entries: Vec<String> = (0..130)
            .map(|number| folder(&format!("F{number:03}="), &format!("F{number:03}"), 0, 0))
            .collect();
        let transport =
            Scripted::new().json(Method::Get, "/mailFolders?", 200, &folders(&entries, None));
        assert_eq!(listing(&api(&transport))?.len(), PAGE_ITEMS);
        Ok(())
    }

    #[test]
    fn a_failing_child_listing_fails_the_whole_listing() -> TestResult {
        for (status, wanted) in [
            (500, "temporary problem"),
            (403, "403"),
            (401, "no longer accepts"),
        ] {
            let transport = Scripted::new()
                .json(Method::Get, "/childFolders", status, "{}")
                .json(
                    Method::Get,
                    "/mailFolders?",
                    200,
                    &folders(&[folder("AAMkInbox=", "Inbox", 0, 1)], None),
                );
            let result = listing(&api(&transport));
            assert!(
                matches!(&result, Err(ToolError::Msg(text)) if text.contains(wanted)),
                "{status}: {result:?}"
            );
        }
        let transport = Scripted::new()
            .reply(Method::Get, "/childFolders", vec![Err(HttpError::TimedOut)])
            .json(
                Method::Get,
                "/mailFolders?",
                200,
                &folders(&[folder("AAMkInbox=", "Inbox", 0, 1)], None),
            );
        assert!(listing(&api(&transport)).is_err());
        let transport = Scripted::new()
            .json(Method::Get, "/childFolders", 200, r#"{"value": 5}"#)
            .json(
                Method::Get,
                "/mailFolders?",
                200,
                &folders(&[folder("AAMkInbox=", "Inbox", 0, 1)], None),
            );
        assert_eq!(listing(&api(&transport)), Err(super::super::unexpected()));
        Ok(())
    }

    #[test]
    fn a_top_level_answer_without_a_list_or_with_a_foreign_link_is_refused() {
        for body in [r#"{}"#, r#"{"value": null}"#, r#"[]"#, "null", ""] {
            let transport = Scripted::new().json(Method::Get, "/mailFolders?", 200, body);
            assert_eq!(
                listing(&api(&transport)),
                Err(super::super::unexpected()),
                "{body:?}"
            );
        }
        let transport = Scripted::new().json(
            Method::Get,
            "/mailFolders?",
            200,
            &folders(
                &[folder("A=", "A", 0, 0)],
                Some("https://evil.example/v1.0/me/mailFolders"),
            ),
        );
        let result = listing(&api(&transport));
        assert!(matches!(result, Err(ToolError::Msg(text)) if text.contains("continuation link")));
        assert_eq!(transport.seen().len(), 1);
    }

    #[test]
    fn a_top_level_link_that_bursts_or_leaves_its_path_is_refused_without_a_request() {
        for link in [
            "https://graph.microsoft.com/v1.0/me/mailFolders?$skip=[1-9999]",
            "https://graph.microsoft.com/v1.0/me/mailFolders?$skip={1,2}",
            "https://graph.microsoft.com/v1.0/me/mailFolders/../messages",
            "https://graph.microsoft.com/v1.0/me/%2e%2e/users",
        ] {
            let transport = Scripted::new().json(
                Method::Get,
                "/mailFolders?",
                200,
                &folders(&[folder("A=", "A", 0, 0)], Some(link)),
            );
            let result = listing(&api(&transport));
            assert!(
                matches!(&result, Err(ToolError::Msg(text)) if text.contains("continuation link")),
                "{link}: {result:?}"
            );
            assert_eq!(transport.seen().len(), 1, "{link}");
        }
    }

    #[test]
    fn no_name_means_the_inbox_and_needs_no_request() -> TestResult {
        for wanted in [None, Some(""), Some("   "), Some("\t")] {
            let transport = Scripted::new();
            let place = resolve(&api(&transport), wanted)?;
            assert_eq!(
                place,
                Place {
                    id: "inbox".to_string(),
                    name: "Inbox".to_string()
                },
                "{wanted:?}"
            );
            assert!(transport.seen().is_empty());
        }
        Ok(())
    }

    #[test]
    fn well_known_names_resolve_without_a_request_whatever_their_case() -> TestResult {
        for (wanted, id, name) in [
            ("inbox", "inbox", "Inbox"),
            ("INBOX", "inbox", "Inbox"),
            ("Inbox/", "inbox", "Inbox"),
            (" /Inbox/ ", "inbox", "Inbox"),
            ("sent", "sentitems", "Sent Items"),
            ("Sent Items", "sentitems", "Sent Items"),
            ("SENTITEMS", "sentitems", "Sent Items"),
            ("sent   items", "sentitems", "Sent Items"),
            ("drafts", "drafts", "Drafts"),
            ("Drafts", "drafts", "Drafts"),
            ("deleted items", "deleteditems", "Deleted Items"),
            ("Deleted Items", "deleteditems", "Deleted Items"),
            ("deleteditems", "deleteditems", "Deleted Items"),
            ("Trash", "deleteditems", "Deleted Items"),
            ("junk", "junkemail", "Junk Email"),
            ("Junk Email", "junkemail", "Junk Email"),
            ("junkemail", "junkemail", "Junk Email"),
            ("SPAM", "junkemail", "Junk Email"),
            ("archive", "archive", "Archive"),
            ("Archive", "archive", "Archive"),
        ] {
            let transport = Scripted::new();
            let place = resolve(&api(&transport), Some(wanted))?;
            assert_eq!(
                (place.id.as_str(), place.name.as_str()),
                (id, name),
                "{wanted:?}"
            );
            assert!(transport.seen().is_empty(), "{wanted:?}");
        }
        Ok(())
    }

    #[test]
    fn other_names_are_looked_up_by_path_or_by_name_ignoring_case() -> TestResult {
        for (wanted, id, name) in [
            ("Inbox/Projects", "AAMkProjects=", "Inbox/Projects"),
            ("inbox/PROJECTS", "AAMkProjects=", "Inbox/Projects"),
            (" inbox / projects ", "AAMkProjects=", "Inbox/Projects"),
            ("/Inbox/Projects/", "AAMkProjects=", "Inbox/Projects"),
            ("Inbox/Projects/2026", "AAMk2026=", "Inbox/Projects/2026"),
            ("projects", "AAMkProjects=", "Inbox/Projects"),
            ("Receipts", "AAMkReceipts=", "Inbox/Receipts"),
            ("2026", "AAMk2026=", "Inbox/Projects/2026"),
        ] {
            let transport = mailbox();
            let place = resolve(&api(&transport), Some(wanted))?;
            assert_eq!(
                (place.id.as_str(), place.name.as_str()),
                (id, name),
                "{wanted:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn unknown_names_are_reported_with_the_standard_text() -> TestResult {
        for wanted in [
            "Nowhere",
            "Inbox/Nowhere",
            "Inbox/Projects/2026/Deeper",
            "Inbox/Projects/2026/Deeper/Deepest",
            "Projects/Inbox",
            "Projects/2026",
            "///",
            " / ",
            "in box",
            "Inbo",
            "Inbox\u{0}",
        ] {
            let transport = mailbox();
            let result = resolve(&api(&transport), Some(wanted));
            assert_eq!(
                result,
                Err(ToolError::msg(
                    "Mailbox not found. Use list_mailboxes to see the valid names."
                )),
                "{wanted:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn the_shallowest_folder_with_a_name_wins_and_whole_paths_beat_names() -> TestResult {
        let transport = Scripted::new()
            .json(
                Method::Get,
                "/mailFolders/AAMkInbox%3D/childFolders",
                200,
                &folders(
                    &[
                        folder("AAMkDeepReceipts=", "Receipts", 0, 0),
                        folder("AAMkLetters=", "Receipts/Letters", 0, 0),
                    ],
                    None,
                ),
            )
            .json(
                Method::Get,
                "/mailFolders?",
                200,
                &folders(
                    &[
                        folder("AAMkInbox=", "Inbox", 0, 1),
                        folder("AAMkTopReceipts=", "Receipts", 0, 0),
                    ],
                    None,
                ),
            );
        let found = |wanted: &str| resolve(&api(&transport), Some(wanted)).map(|place| place.id);
        assert_eq!(found("Receipts")?, "AAMkTopReceipts=");
        assert_eq!(found("Inbox/Receipts")?, "AAMkDeepReceipts=");
        assert_eq!(found("inbox/receipts/letters")?, "AAMkLetters=");
        Ok(())
    }

    #[test]
    fn a_failing_folder_listing_fails_the_lookup() {
        for status in [401, 403, 500] {
            let transport = Scripted::new().json(Method::Get, "/mailFolders?", status, "{}");
            assert!(matches!(
                resolve(&api(&transport), Some("Projects")),
                Err(ToolError::Msg(_))
            ));
        }
    }

    #[test]
    fn the_well_known_table_has_unique_lower_case_aliases_and_the_inbox_first() {
        let first = WELL_KNOWN.first();
        assert_eq!(first.map(|(id, _, _)| *id), Some("inbox"));
        let mut aliases: Vec<&str> = WELL_KNOWN
            .iter()
            .flat_map(|(_, _, names)| names.iter().copied())
            .collect();
        let count = aliases.len();
        aliases.sort_unstable();
        aliases.dedup();
        assert_eq!(aliases.len(), count);
        for alias in &aliases {
            assert_eq!(normalize(alias), *alias);
        }
        let ids: Vec<&str> = WELL_KNOWN.iter().map(|(id, _, _)| *id).collect();
        assert_eq!(
            ids,
            [
                "inbox",
                "sentitems",
                "drafts",
                "deleteditems",
                "junkemail",
                "archive"
            ]
        );
    }

    #[test]
    fn names_are_compared_after_the_same_cleaning() {
        assert_eq!(normalize("  A  B / c\td  /"), "a b/c d");
        assert_eq!(normalize("\u{c4}/\u{d6}"), "\u{e4}/\u{f6}");
        assert_eq!(normalize(""), "");
        assert_eq!(normalize("//"), "");
    }
}
