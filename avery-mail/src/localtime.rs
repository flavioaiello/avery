use std::fs;
use std::process::Command;

use crate::civil::civil_from_days;

const DATE: &str = "/bin/date";

const LOCALTIME: &str = "/etc/localtime";

fn zone_name(name: &str) -> Option<String> {
    let name = name.trim_start_matches(':');
    (!name.is_empty()
        && name.len() <= 64
        && !name.starts_with('/')
        && !name.contains("..")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_+-/".contains(c)))
    .then(|| name.to_string())
}

pub(crate) fn zone_from(variable: Option<&str>, link: Option<&str>) -> Option<String> {
    if let Some(name) = variable.filter(|name| !name.is_empty()) {
        return zone_name(name);
    }
    let target = link?;
    zone_name(target.split_once("zoneinfo/")?.1)
}

pub(crate) fn zone() -> Option<String> {
    let variable = std::env::var("TZ").ok();
    let link = fs::read_link(LOCALTIME)
        .ok()
        .and_then(|target| target.to_str().map(str::to_string));
    zone_from(variable.as_deref(), link.as_deref())
}

pub(crate) fn minute(seconds: i64) -> String {
    local(seconds).unwrap_or_else(|| utc(seconds))
}

fn local(seconds: i64) -> Option<String> {
    let mut command = Command::new(DATE);
    if cfg!(target_os = "macos") {
        command.arg("-r").arg(seconds.to_string());
    } else {
        command.arg("-d").arg(format!("@{seconds}"));
    }
    let output = command.arg("+%Y-%m-%d %H:%M").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (text.len() == 16).then(|| text.to_string())
}

pub(crate) fn utc(seconds: i64) -> String {
    let (year, month, day) = civil_from_days(seconds.div_euclid(86_400));
    let rest = seconds.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        rest / 3600,
        rest % 3600 / 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_fallback_formats_known_instants() {
        assert_eq!(utc(0), "1970-01-01 00:00");
        assert_eq!(utc(951_782_400), "2000-02-29 00:00");
        assert_eq!(utc(1_700_000_000), "2023-11-14 22:13");
        assert_eq!(utc(-1), "1969-12-31 23:59");
        assert_eq!(utc(4_102_444_800), "2100-01-01 00:00");
    }

    #[test]
    fn the_zone_comes_from_tz_or_the_localtime_link() {
        assert_eq!(
            zone_from(None, Some("/var/db/timezone/zoneinfo/Europe/Zurich")),
            Some("Europe/Zurich".to_string())
        );
        assert_eq!(
            zone_from(
                None,
                Some("/usr/share/zoneinfo/America/Argentina/Buenos_Aires")
            ),
            Some("America/Argentina/Buenos_Aires".to_string())
        );
        assert_eq!(zone_from(Some("UTC"), None), Some("UTC".to_string()));
        assert_eq!(
            zone_from(Some(":Europe/Berlin"), None),
            Some("Europe/Berlin".to_string())
        );
        assert_eq!(
            zone_from(
                Some("Etc/GMT+1"),
                Some("/var/db/timezone/zoneinfo/Europe/Zurich")
            ),
            Some("Etc/GMT+1".to_string())
        );
        assert_eq!(
            zone_from(Some(""), Some("/x/zoneinfo/Asia/Tokyo")),
            Some("Asia/Tokyo".to_string())
        );
        for bad in [
            (Some("CET-1CEST,M3.5.0,M10.5.0/3"), None),
            (Some("../etc/passwd"), None),
            (Some("/etc/passwd"), None),
            (Some("Europe/Zurich\n"), None),
            (None, Some("/etc/nothing")),
            (None, Some("/x/zoneinfo/")),
            (None, Some("/x/zoneinfo/a b")),
            (None, None),
        ] {
            assert_eq!(zone_from(bad.0, bad.1), None, "{bad:?}");
        }
    }

    #[test]
    fn local_time_has_the_expected_shape() {
        let text = minute(1_700_000_000);
        assert_eq!(text.len(), 16);
        assert_eq!(text.as_bytes().get(4), Some(&b'-'));
        assert_eq!(text.as_bytes().get(13), Some(&b':'));
    }
}
