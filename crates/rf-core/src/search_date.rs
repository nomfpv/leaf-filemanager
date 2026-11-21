//! The modified-date filter of the search in a folder: `modified:2026-11-20`, `modified:2026-11` (a month), `modified:today|yesterday|week|month`.
//! Pure: the day is a parameter, so the table test needs no clock.

use chrono::{Datelike, Duration, NaiveDate, TimeZone};

/// The days (inclusive) a file's modification day has to fall in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DayRange {
    pub first: NaiveDate,
    pub last: NaiveDate,
}

impl DayRange {
    pub fn contains(&self, day: NaiveDate) -> bool {
        day >= self.first && day <= self.last
    }

    /// Whether a modification time (milliseconds since 1970, in the computer's time zone) falls in the range.
    pub fn contains_ms(&self, ms: i64) -> bool {
        match chrono::Local.timestamp_millis_opt(ms) {
            chrono::LocalResult::Single(t) => self.contains(t.date_naive()),
            _ => false,
        }
    }
}

/// A query split into what is looked for in names and the date filter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Split {
    /// The query without its `modified:` word.
    pub text: String,
    pub modified: Option<DayRange>,
}

/// What a value after `modified:` means on `today`: a date, `today`, `yesterday`, `week` (this week, from Monday) or
/// `month` (this month). `None` for anything else.
pub fn range_of(value: &str, today: NaiveDate) -> Option<DayRange> {
    let one = |d: NaiveDate| Some(DayRange { first: d, last: d });
    match value.to_lowercase().as_str() {
        "today" => one(today),
        "yesterday" => one(today - Duration::days(1)),
        "week" => Some(DayRange { first: today - Duration::days(today.weekday().num_days_from_monday() as i64), last: today }),
        "month" => Some(DayRange { first: today.with_day(1)?, last: today }),
        v if v.len() == 7 => {
            // a whole month: `2026-11`
            let first = NaiveDate::parse_from_str(&format!("{v}-01"), "%Y-%m-%d").ok()?;
            let next = if first.month() == 12 { NaiveDate::from_ymd_opt(first.year() + 1, 1, 1)? } else { first.with_month(first.month() + 1)? };
            Some(DayRange { first, last: next - Duration::days(1) })
        }
        v => one(NaiveDate::parse_from_str(v, "%Y-%m-%d").ok()?),
    }
}

/// Takes the first `modified:<value>` word out of `query`. A word with a value that means nothing stays in the text.
pub fn split(query: &str, today: NaiveDate) -> Split {
    let mut modified = None;
    let mut kept: Vec<&str> = Vec::new();
    for word in query.split_whitespace() {
        let range = (modified.is_none()).then(|| word.get(..9).filter(|k| k.eq_ignore_ascii_case("modified:")).and_then(|_| range_of(&word[9..], today))).flatten();
        match range {
            Some(r) => modified = Some(r),
            None => kept.push(word),
        }
    }
    Split { text: kept.join(" "), modified }
}

/// The query with its `modified:` word replaced by `value` (`None`: taken out): what the "Modified" chips write.
pub fn with_modified(query: &str, value: Option<&str>) -> String {
    let mut words: Vec<&str> = query.split_whitespace().filter(|w| !w.get(..9).is_some_and(|k| k.eq_ignore_ascii_case("modified:"))).collect();
    let word = value.map(|v| format!("modified:{v}"));
    if let Some(w) = &word {
        words.push(w);
    }
    words.join(" ")
}

/// The value of the query's `modified:` word, when it has one that means something.
pub fn current(query: &str, today: NaiveDate) -> Option<String> {
    query.split_whitespace().find_map(|w| w.get(..9).filter(|k| k.eq_ignore_ascii_case("modified:")).and_then(|_| range_of(&w[9..], today).map(|_| w[9..].to_lowercase())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn the_words_make_the_days() {
        // 2026-11-20 is a Friday
        let today = d(2026, 11, 20);
        let r = |a, b| Some(DayRange { first: a, last: b });
        let table: &[(&str, Option<DayRange>)] = &[
            ("today", r(today, today)),
            ("Today", r(today, today)),
            ("yesterday", r(d(2026, 11, 19), d(2026, 11, 19))),
            ("week", r(d(2026, 11, 16), today)),
            ("month", r(d(2026, 11, 1), today)),
            ("2026-11-02", r(d(2026, 11, 2), d(2026, 11, 2))),
            ("2026-13-02", None),
            ("tomorrow", None),
            ("", None),
        ];
        for (word, want) in table {
            assert_eq!(range_of(word, today), *want, "{word}");
        }
    }

    #[test]
    fn the_query_is_split_into_text_and_filter() {
        let today = d(2026, 11, 20);
        let day = Some(DayRange { first: today, last: today });
        let table: &[(&str, &str, Option<DayRange>)] = &[
            ("modified:today", "", day),
            ("report modified:today", "report", day),
            ("MODIFIED:today report", "report", day),
            ("report", "report", None),
            // a word that means nothing is looked for in names like any other
            ("modified:soon report", "modified:soon report", None),
            ("modified:", "modified:", None),
            ("a  b", "a b", None),
        ];
        for (query, text, modified) in table {
            assert_eq!(split(query, today), Split { text: text.to_string(), modified: *modified }, "{query}");
        }
    }

    #[test]
    fn the_chips_write_and_read_the_word() {
        let today = d(2026, 11, 20);
        assert_eq!(with_modified("report", Some("week")), "report modified:week");
        assert_eq!(with_modified("report modified:today", Some("month")), "report modified:month");
        assert_eq!(with_modified("report modified:today", None), "report");
        assert_eq!(current("report modified:Week", today).as_deref(), Some("week"));
        assert_eq!(current("report modified:soon", today), None);
        assert!(DayRange { first: today, last: today }.contains(today));
        assert!(!DayRange { first: today, last: today }.contains(d(2026, 11, 21)));
    }
}
