use std::process::Command;

const DATE: &str = "/bin/date";

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
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
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
    fn local_time_has_the_expected_shape() {
        let text = minute(1_700_000_000);
        assert_eq!(text.len(), 16);
        assert_eq!(text.as_bytes().get(4), Some(&b'-'));
        assert_eq!(text.as_bytes().get(13), Some(&b':'));
    }
}
