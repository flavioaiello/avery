use std::time::Duration;

use crate::error::{ToolError, ToolResult};
use crate::http::{self, Method, Request, Transport};
use crate::json::{self, Json, Object};
use crate::provider::{account_id, OAuth, Provider};
use crate::text::one_line_str;
use crate::vault::Vault;

const TOKEN_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_TOKEN: usize = 8192;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Record {
    pub(crate) client_id: String,
    pub(crate) client_secret: Option<String>,
    pub(crate) refresh_token: String,
    pub(crate) scope: String,
}

fn text_field(object: &Object, key: &str) -> Option<String> {
    match object.get(key) {
        Some(Json::Str(text)) if !text.is_empty() && text.len() <= MAX_TOKEN => Some(text.clone()),
        _ => None,
    }
}

impl Record {
    pub(crate) fn to_json(&self) -> String {
        let mut object = Object::new();
        object.insert("client_id", self.client_id.as_str());
        if let Some(secret) = &self.client_secret {
            object.insert("client_secret", secret.as_str());
        }
        object.insert("refresh_token", self.refresh_token.as_str());
        object.insert("scope", self.scope.as_str());
        json::dumps(&Json::Obj(object), true)
    }

    pub(crate) fn parse(text: &str) -> Option<Record> {
        let Ok(Json::Obj(object)) = json::parse(text) else {
            return None;
        };
        if object
            .keys()
            .any(|key| !["client_id", "client_secret", "refresh_token", "scope"].contains(&key))
        {
            return None;
        }
        let client_secret = match object.get("client_secret") {
            None => None,
            Some(_) => Some(text_field(&object, "client_secret")?),
        };
        Some(Record {
            client_id: text_field(&object, "client_id")?,
            client_secret,
            refresh_token: text_field(&object, "refresh_token")?,
            scope: match object.get("scope") {
                Some(Json::Str(scope)) if scope.len() <= MAX_TOKEN => scope.clone(),
                _ => return None,
            },
        })
    }

    pub(crate) fn has(&self, wanted: &str) -> bool {
        self.scope
            .split_whitespace()
            .any(|granted| granted.eq_ignore_ascii_case(wanted))
    }

    fn has_all(&self, wanted: &[&str]) -> bool {
        !wanted.is_empty() && wanted.iter().all(|scope| self.has(scope))
    }

    pub(crate) fn can_write_mail(&self, provider: Provider) -> bool {
        provider
            .oauth()
            .is_some_and(|oauth| self.has_all(oauth.mail_write))
    }

    pub(crate) fn can_write_calendar(&self, provider: Provider) -> bool {
        provider
            .oauth()
            .is_some_and(|oauth| self.has_all(oauth.calendar_write))
    }
}

pub(crate) fn relink(provider: Provider) -> String {
    format!(
        "Ask the user to run `avery mail link {}` again.",
        provider.as_str()
    )
}

pub(crate) fn load_record(
    vault: &dyn Vault,
    provider: Provider,
    address: &str,
) -> ToolResult<Record> {
    let name = account_id(provider, address)
        .ok_or_else(|| ToolError::msg("This account has no valid address."))?;
    let stored = vault.load(&name).map_err(|error| {
        ToolError::msg(format!(
            "The Keychain is not available ({error}). Ask the user to unlock the Mac and retry."
        ))
    })?;
    let Some(text) = stored else {
        return Err(ToolError::msg(format!(
            "This account is not linked (no sign-in is stored). {}",
            relink(provider)
        )));
    };
    Record::parse(&text).ok_or_else(|| {
        ToolError::msg(format!(
            "The stored sign-in of this account is unreadable. {}",
            relink(provider)
        ))
    })
}

fn refresh_form(oauth: &OAuth, record: &Record) -> String {
    let mut pairs: Vec<(&str, &str)> = vec![
        ("client_id", record.client_id.as_str()),
        ("grant_type", "refresh_token"),
        ("refresh_token", record.refresh_token.as_str()),
    ];
    if let Some(secret) = &record.client_secret {
        pairs.push(("client_secret", secret.as_str()));
    }
    if oauth.scope_on_refresh {
        pairs.push(("scope", record.scope.as_str()));
    }
    http::query(&pairs)
}

pub(crate) fn access_token(
    transport: &dyn Transport,
    vault: &dyn Vault,
    provider: Provider,
    address: &str,
) -> ToolResult<String> {
    let oauth = provider
        .oauth()
        .ok_or_else(|| ToolError::internal("provider"))?;
    let record = load_record(vault, provider, address)?;
    let request = Request::new(Method::Post, oauth.token, TOKEN_TIMEOUT)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(&refresh_form(oauth, &record));
    let response = transport.send(&request).map_err(|_| {
        ToolError::msg("The sign-in service could not be reached. Retry in a moment.")
    })?;
    let answer = json::parse(&response.body).ok();
    let object = answer.as_ref().and_then(Json::as_object);
    if response.status != 200 {
        let code = object
            .and_then(|object| object.get("error"))
            .and_then(Json::as_str)
            .unwrap_or_default();
        if code == "invalid_grant" || response.status == 401 {
            return Err(ToolError::msg(format!(
                "The sign-in of this account has expired or was revoked. {}",
                relink(provider)
            )));
        }
        if response.status >= 500 || response.status == 429 {
            return Err(ToolError::msg(
                "The sign-in service is busy. Retry in a moment.",
            ));
        }
        return Err(ToolError::msg(format!(
            "The sign-in could not be renewed (error {}). {}",
            one_line_str(code, 40),
            relink(provider)
        )));
    }
    let Some(object) = object else {
        return Err(ToolError::msg(
            "The sign-in service gave an unexpected answer.",
        ));
    };
    let Some(token) = text_field(object, "access_token") else {
        return Err(ToolError::msg(
            "The sign-in service gave an unexpected answer.",
        ));
    };
    if let Some(next) = text_field(object, "refresh_token") {
        if next != record.refresh_token {
            let updated = Record {
                refresh_token: next,
                ..record
            };
            if let Some(name) = account_id(provider, address) {
                vault.store(&name, &updated.to_json()).map_err(|error| {
                    ToolError::msg(format!(
                        "The renewed sign-in could not be stored in the Keychain ({error})."
                    ))
                })?;
            }
        }
    }
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::scripted::Scripted;
    use crate::http::{HttpError, Response};
    use crate::testutil::TestResult;
    use crate::vault::memory::Memory;

    const GOOGLE_TOKEN: &str = "https://oauth2.googleapis.com/token";
    const MICROSOFT_TOKEN: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
    const GMAIL: &str = "google:alice@gmail.example";
    const OUTLOOK: &str = "microsoft:bob@outlook.example";

    fn google_record() -> Record {
        Record {
            client_id: "cid.apps.example".into(),
            client_secret: Some("csecret".into()),
            refresh_token: "1//refresh".into(),
            scope: "openid email https://www.googleapis.com/auth/gmail.readonly https://www.googleapis.com/auth/calendar.readonly".into(),
        }
    }

    fn microsoft_record() -> Record {
        Record {
            client_id: "ms-client".into(),
            client_secret: None,
            refresh_token: "M.R3_old".into(),
            scope: "offline_access User.Read Mail.Read Calendars.Read".into(),
        }
    }

    fn token_reply(body: &str) -> Scripted {
        Scripted::new().json(Method::Post, "/token", 200, body)
    }

    #[test]
    fn records_round_trip_and_refuse_anything_odd() -> TestResult {
        let record = google_record();
        assert_eq!(Record::parse(&record.to_json()), Some(record));
        let plain = microsoft_record();
        assert_eq!(Record::parse(&plain.to_json()), Some(plain.clone()));
        assert!(!plain.to_json().contains("client_secret"));
        for bad in [
            "",
            "[]",
            "{}",
            r#"{"client_id": "a", "refresh_token": "b"}"#,
            r#"{"client_id": "a", "refresh_token": "b", "scope": 5}"#,
            r#"{"client_id": "", "refresh_token": "b", "scope": ""}"#,
            r#"{"client_id": "a", "refresh_token": "b", "scope": "", "extra": 1}"#,
            r#"{"client_id": "a", "client_secret": 3, "refresh_token": "b", "scope": ""}"#,
        ] {
            assert_eq!(Record::parse(bad), None, "{bad}");
        }
        let huge = format!(
            r#"{{"client_id": "a", "refresh_token": "{}", "scope": ""}}"#,
            "x".repeat(MAX_TOKEN + 1)
        );
        assert_eq!(Record::parse(&huge), None);
        Ok(())
    }

    #[test]
    fn write_scopes_are_recognised_per_area() {
        let mut google = google_record();
        assert!(!google.can_write_mail(Provider::Google));
        assert!(!google.can_write_calendar(Provider::Google));
        google
            .scope
            .push_str(" https://www.googleapis.com/auth/gmail.compose");
        assert!(google.can_write_mail(Provider::Google));
        assert!(!google.can_write_calendar(Provider::Google));
        google
            .scope
            .push_str(" https://www.googleapis.com/auth/calendar.events");
        assert!(google.can_write_calendar(Provider::Google));
        let mut microsoft = microsoft_record();
        assert!(!microsoft.can_write_mail(Provider::Microsoft));
        microsoft.scope.push_str(" Mail.ReadWrite mail.send");
        assert!(microsoft.can_write_mail(Provider::Microsoft));
        assert!(!microsoft.can_write_calendar(Provider::Microsoft));
        microsoft.scope.push_str(" calendars.readwrite");
        assert!(microsoft.can_write_calendar(Provider::Microsoft));
        assert!(!microsoft.can_write_mail(Provider::Apple));
    }

    #[test]
    fn a_google_token_is_renewed_with_the_client_secret() -> TestResult {
        let vault = Memory::new().with(GMAIL, &google_record().to_json());
        let transport = token_reply(
            r#"{"access_token": "ya29.token", "expires_in": 3599, "token_type": "Bearer"}"#,
        );
        let token = access_token(&transport, &vault, Provider::Google, "alice@gmail.example")?;
        assert_eq!(token, "ya29.token");
        let seen = transport.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].url, GOOGLE_TOKEN);
        let body = seen[0].body.clone().ok_or("body")?;
        assert_eq!(
            body,
            "client_id=cid.apps.example&grant_type=refresh_token&refresh_token=1%2F%2Frefresh&client_secret=csecret"
        );
        assert_eq!(vault.get(GMAIL), Some(google_record().to_json()));
        Ok(())
    }

    #[test]
    fn a_rotated_microsoft_refresh_token_is_stored() -> TestResult {
        let vault = Memory::new().with(OUTLOOK, &microsoft_record().to_json());
        let transport = token_reply(
            r#"{"access_token": "EwB.token", "refresh_token": "M.R3_new", "scope": "x", "expires_in": 3600}"#,
        );
        let token = access_token(
            &transport,
            &vault,
            Provider::Microsoft,
            "bob@outlook.example",
        )?;
        assert_eq!(token, "EwB.token");
        let seen = transport.seen();
        assert_eq!(seen[0].url, MICROSOFT_TOKEN);
        let body = seen[0].body.clone().ok_or("body")?;
        assert!(body.contains("scope=offline_access%20User.Read%20Mail.Read%20Calendars.Read"));
        assert!(!body.contains("client_secret"));
        let stored = Record::parse(&vault.get(OUTLOOK).ok_or("stored")?).ok_or("record")?;
        assert_eq!(stored.refresh_token, "M.R3_new");
        assert_eq!(stored.client_id, "ms-client");
        assert_eq!(stored.scope, microsoft_record().scope);
        Ok(())
    }

    #[test]
    fn problems_are_explained_and_name_the_way_out() -> TestResult {
        let vault = Memory::new().with(GMAIL, &google_record().to_json());
        let cases: Vec<(Scripted, &str)> = vec![
            (
                Scripted::new().json(Method::Post, "/token", 400, r#"{"error": "invalid_grant"}"#),
                "expired or was revoked",
            ),
            (
                Scripted::new().json(Method::Post, "/token", 401, "{}"),
                "expired or was revoked",
            ),
            (
                Scripted::new().json(
                    Method::Post,
                    "/token",
                    400,
                    r#"{"error": "invalid_client\nx"}"#,
                ),
                "error invalid_client x",
            ),
            (
                Scripted::new().json(Method::Post, "/token", 503, "oops"),
                "busy",
            ),
            (
                Scripted::new().reply(Method::Post, "/token", vec![Err(HttpError::TimedOut)]),
                "could not be reached",
            ),
            (
                Scripted::new().json(Method::Post, "/token", 200, "not json"),
                "unexpected answer",
            ),
            (
                Scripted::new().json(Method::Post, "/token", 200, r#"{"access_token": ""}"#),
                "unexpected answer",
            ),
            (
                Scripted::new().reply(
                    Method::Post,
                    "/token",
                    vec![Ok(Response {
                        status: 200,
                        body: "[]".into(),
                    })],
                ),
                "unexpected answer",
            ),
        ];
        for (transport, wanted) in cases {
            let Err(error) =
                access_token(&transport, &vault, Provider::Google, "alice@gmail.example")
            else {
                return Err("expected an error".into());
            };
            assert!(error.message().contains(wanted), "{error}");
            if wanted != "busy" && wanted != "could not be reached" && wanted != "unexpected answer"
            {
                assert!(
                    error.message().contains("avery mail link google"),
                    "{error}"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn missing_unreadable_and_unreachable_vault_entries_are_reported() -> TestResult {
        let transport = Scripted::new();
        let empty = Memory::new();
        let broken = Memory::broken();
        let garbled = Memory::new().with(GMAIL, "not json");
        for (vault, wanted) in [
            (&empty, "not linked"),
            (&broken, "Keychain is not available"),
            (&garbled, "unreadable"),
        ] {
            let Err(error) =
                access_token(&transport, vault, Provider::Google, "alice@gmail.example")
            else {
                return Err("expected an error".into());
            };
            assert!(error.message().contains(wanted), "{error}");
        }
        assert!(access_token(&transport, &empty, Provider::Google, "nonsense").is_err());
        assert!(access_token(&transport, &empty, Provider::Apple, "alice@gmail.example").is_err());
        assert!(transport.seen().is_empty());
        Ok(())
    }

    #[test]
    fn a_failing_vault_write_after_rotation_is_an_error() -> TestResult {
        let vault = Memory::new().with(OUTLOOK, &microsoft_record().to_json());
        let transport = token_reply(r#"{"access_token": "t", "refresh_token": "new"}"#);
        let read_only = Memory::new()
            .with(OUTLOOK, &microsoft_record().to_json())
            .read_only();
        let Err(error) = access_token(
            &transport,
            &read_only,
            Provider::Microsoft,
            "bob@outlook.example",
        ) else {
            return Err("expected an error".into());
        };
        assert!(error.message().contains("could not be stored"), "{error}");
        assert!(access_token(
            &transport,
            &vault,
            Provider::Microsoft,
            "bob@outlook.example"
        )
        .is_ok());
        Ok(())
    }
}
