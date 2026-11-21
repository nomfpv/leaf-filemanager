//! Linux backend of rf_win::datefmt. Status: REAL (a small built-in table by locale; no ICU)
//!
//! The short date order, separator and 12/24 hour clock of the user's time locale (`LC_ALL`, `LC_TIME`, `LANG`). A locale the table does
//! not know, and the C locale, answer `None`: the app then writes its own default format.

use chrono::{DateTime, Datelike, Local, Timelike};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Order {
    Dmy,
    Mdy,
    Ymd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Style {
    order: Order,
    sep: &'static str,
    /// Day and month with two digits.
    pad: bool,
    /// What follows the last number of the date ("." for Korean and Hungarian dates).
    tail: &'static str,
    hour12: bool,
}

const fn st(order: Order, sep: &'static str, pad: bool, hour12: bool) -> Style {
    Style { order, sep, pad, tail: "", hour12 }
}

fn style_of(lang: &str, region: &str) -> Option<Style> {
    use Order::*;
    Some(match (lang, region) {
        ("en", "US" | "PH" | "") => st(Mdy, "/", false, true),
        ("en", "CA") => st(Ymd, "-", true, true),
        ("en", "GB" | "IE") => st(Dmy, "/", true, false),
        ("en", "AU" | "NZ" | "IN") => st(Dmy, "/", true, true),
        ("en", "ZA") => st(Ymd, "/", true, false),
        ("en", _) => st(Dmy, "/", true, false),
        ("de" | "ru" | "pl" | "tr" | "fi" | "nb" | "nn" | "no" | "cs" | "sk" | "da" | "uk" | "ro" | "bg" | "hr" | "sl" | "et" | "is", _) => st(Dmy, ".", true, false),
        ("fr" | "es" | "it" | "pt" | "ca" | "gl" | "id" | "vi", _) => st(Dmy, "/", true, false),
        ("el", _) => st(Dmy, "/", false, true),
        ("nl", _) => st(Dmy, "-", true, false),
        ("sv" | "lt", _) => st(Ymd, "-", true, false),
        ("ja" | "zh", _) => st(Ymd, "/", false, false),
        ("ko", _) => Style { order: Ymd, sep: ". ", pad: true, tail: ".", hour12: true },
        ("hu", _) => Style { order: Ymd, sep: ". ", pad: true, tail: ".", hour12: false },
        _ => return None,
    })
}

fn write(style: Style, y: i32, mo: u32, d: u32, h: u32, mi: u32) -> String {
    let n = |v: u32| if style.pad { format!("{v:02}") } else { v.to_string() };
    let (dd, mm) = (n(d), n(mo));
    let date = match style.order {
        Order::Dmy => format!("{dd}{s}{mm}{s}{y}", s = style.sep),
        Order::Mdy => format!("{mm}{s}{dd}{s}{y}", s = style.sep),
        Order::Ymd => format!("{y}{s}{mm}{s}{dd}", s = style.sep),
    };
    let time = if style.hour12 {
        let h12 = if h % 12 == 0 { 12 } else { h % 12 };
        format!("{h12}:{mi:02} {}", if h < 12 { "AM" } else { "PM" })
    } else {
        format!("{h:02}:{mi:02}")
    };
    format!("{date}{} {time}", style.tail)
}

pub(crate) fn short_for_locale(name: &str, dt: &DateTime<Local>) -> Option<String> {
    let (lang, region) = crate::locale::split(name);
    let style = style_of(&lang, &region)?;
    Some(write(style, dt.year(), dt.month(), dt.day(), dt.hour(), dt.minute()))
}

/// "17.11.2026 09:30" or "11/17/2026 9:30 AM" as the user's time locale writes it; `None` for a locale the table does not know.
pub fn short_date_time(dt: &DateTime<Local>) -> Option<String> {
    short_for_locale(&crate::locale::time_locale()?, dt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn dates_follow_the_locale_table() {
        let dt = Local.with_ymd_and_hms(2026, 11, 7, 15, 5, 0).unwrap();
        assert_eq!(short_for_locale("en_US.UTF-8", &dt).unwrap(), "11/7/2026 3:05 PM");
        assert_eq!(short_for_locale("de_DE.UTF-8", &dt).unwrap(), "07.11.2026 15:05");
        assert_eq!(short_for_locale("en_GB.UTF-8", &dt).unwrap(), "07/11/2026 15:05");
        assert_eq!(short_for_locale("sv_SE", &dt).unwrap(), "2026-11-07 15:05");
        assert_eq!(short_for_locale("hu_HU.UTF-8", &dt).unwrap(), "2026. 11. 07. 15:05");
        let midnight = Local.with_ymd_and_hms(2026, 1, 2, 0, 30, 0).unwrap();
        assert_eq!(short_for_locale("en_US", &midnight).unwrap(), "1/2/2026 12:30 AM");
        assert_eq!(short_for_locale("xx_YY", &dt), None);
    }
}
