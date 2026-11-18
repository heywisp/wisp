use wisp_render::transcript::copy;

pub(super) fn view(
    zone: &jiff::tz::TimeZone,
    now: i64,
    at: i64,
) -> wisp_render::plan_usage::ResetTime {
    let seconds = at.saturating_sub(now);
    let relative = match seconds {
        ..=0 => "reset due".into(),
        1..60 => "in less than a minute".into(),
        60..3600 => format!("in {}m", seconds / 60),
        3600..86400 => format!("in {}h {}m", seconds / 3600, seconds % 3600 / 60),
        _ => format!("in {}d {}h", seconds / 86400, seconds % 86400 / 3600),
    };
    let local = |format: &str| {
        jiff::Timestamp::from_second(at)
            .map(|time| time.to_zoned(zone.clone()).strftime(format).to_string())
            .unwrap_or_else(|_| copy::TIME_UNAVAILABLE.into())
    };
    let (brief, long) = match seconds {
        ..86400 => {
            let clock = local("%H:%M");
            let left = seconds.max(60) / 60;
            let countdown = match (left / 60, left % 60) {
                (0, minutes) => format!("in {minutes} min"),
                (hours, 0) => format!("in {hours} h"),
                (hours, minutes) => format!("in {hours} h {minutes} min"),
            };
            (clock.clone(), format!("{clock}, {countdown}"))
        }
        _ => (local("%a"), local("%a %H:%M")),
    };
    wisp_render::plan_usage::ResetTime {
        short: format!("{relative} · {}", local("%a %H:%M")),
        brief,
        long,
    }
}

pub(super) fn absolute(zone: &jiff::tz::TimeZone, at: i64) -> String {
    jiff::Timestamp::from_second(at)
        .map(|time| {
            time.to_zoned(zone.clone())
                .strftime("%a %-d %b %H:%M %:z")
                .to_string()
        })
        .unwrap_or_else(|_| copy::TIME_UNAVAILABLE.into())
}

pub(super) fn spend_day(index: usize, day: &str) -> String {
    match index {
        0 => copy::TODAY.into(),
        1 => copy::YESTERDAY.into(),
        _ => day
            .parse::<jiff::civil::Date>()
            .map(|date| date.strftime("%a").to_string())
            .unwrap_or_else(|_| day.to_owned()),
    }
}

pub(super) fn spend_window(since: Option<&str>) -> String {
    since.map_or_else(
        || format!("{} days", wisp_protocol::SPEND_WINDOW_DAYS),
        |day| {
            let date = day
                .parse::<jiff::civil::Date>()
                .map(|date| date.strftime("%-d %b").to_string())
                .unwrap_or_else(|_| day.to_owned());
            format!("{} {date}", copy::SINCE)
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_views_include_local_offset_and_relative_countdown() {
        let now = "2026-10-30T08:00:00Z"
            .parse::<jiff::Timestamp>()
            .unwrap()
            .as_second();
        let at = "2026-11-05T17:25:54Z"
            .parse::<jiff::Timestamp>()
            .unwrap()
            .as_second();
        let zone = jiff::tz::TimeZone::fixed(jiff::tz::Offset::from_hours(8).unwrap());
        let time = view(&zone, now, at);
        assert_eq!(absolute(&zone, at), "Fri 6 Nov 01:25 +08:00");
        assert_eq!(time.short, "in 6d 9h · Fri 01:25");
        assert_eq!(
            (time.brief.as_str(), time.long.as_str()),
            ("Fri", "Fri 01:25")
        );
        assert_eq!(view(&zone, at, at).short, "reset due · Fri 01:25");
        let soon = view(&zone, at - 7800, at);
        assert_eq!(
            (soon.brief.as_str(), soon.long.as_str()),
            ("01:25", "01:25, in 2 h 10 min")
        );
        assert_eq!(view(&zone, at - 600, at).long, "01:25, in 10 min");
        assert_eq!(view(&zone, at - 3600, at).long, "01:25, in 1 h");
    }
}
