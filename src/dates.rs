//! Recognise the date and date-time formats CSV exports use and rewrite them as
//! ISO 8601: `YYYY-MM-DD`, or `YYYY-MM-DD HH:MM:SS` when there is a time.
//!
//! The date rules are abp's `clean_csv` (C:\code\python\boilerplate\abp.py):
//! - d/m/y versus m/d/y is settled once per column from the data, never
//!   guessed: a first part over 12 proves day-first, a second part over 12
//!   proves month-first, both at once is a mixed column (refused), neither is
//!   ambiguous (the user has to say).
//! - Named months (`25-Apr-2026`, `Apr 25, 2026`) can only be read one way,
//!   so they never vote.
//! - The last group of a d/m/y date is the year; two-digit years under 70 are
//!   20xx, the rest 19xx. Excel serial numbers and anything else are left alone.
//!
//! Beyond abp:
//! - Year-first dates (`2026/09/25`, `2023-31-12`) are read year-month-day,
//!   the ISO order, unless the column proves year-day-month the same way: a
//!   middle part over 12. Both proven at once is a mixed column (refused).
//! - A value shaped like a date that can't be one in any reading (`20/20/2000`,
//!   `2026-02-30`) is reported, and left as it is.
//! - The time is normalised: 12- or 24-hour, with or without seconds, becomes
//!   24-hour `HH:MM:SS`, keeping fractional seconds and a timezone offset (as
//!   `Z` or `±HH:MM`).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    DayFirst,
    MonthFirst,
}

/// How to read a column's dates: the order of its d/m/y values (`None` leaves
/// them as they are) and whether its year-first values are year-day-month.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Plan {
    pub dmy: Option<Order>,
    pub ydm: bool,
}

/// What a column's d/m/y values say about their order.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// No d/m/y values that settle or need an order.
    NoQuestion,
    /// One order proven by `count` values, `evidence` being the first of them.
    Proven { order: Order, evidence: String, count: usize },
    /// d/m/y values with both parts 12 or less: the data can't tell.
    Ambiguous { example: String },
    /// Values proving each order: converting would mean guessing.
    Mixed { dayfirst: String, monthfirst: String },
}

/// What a column's year-first values say about their order.
#[derive(Debug, Clone, PartialEq)]
pub enum YearFirstVerdict {
    /// Year-month-day, the ISO order; it needs no proof.
    Ymd,
    /// Year-day-month, proven by `count` values with a middle part over 12.
    Ydm { evidence: String, count: usize },
    /// Values proving each order: converting would mean guessing.
    Mixed { ymd: String, ydm: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Analysis {
    pub verdict: Verdict,
    pub year_first: YearFirstVerdict,
    /// Values the conversion would change.
    pub convert: usize,
    /// Dates already in ISO form.
    pub already: usize,
    /// Values shaped like dates that can't be real dates (left as they are).
    pub impossible: usize,
    pub impossible_example: Option<String>,
    /// Non-empty values that aren't dates at all (left as they are).
    pub left: usize,
    pub left_example: Option<String>,
    /// Up to three values that would change, one per kind of format.
    pub examples: Vec<String>,
}

impl Analysis {
    /// Whether either order question makes converting a guess.
    pub fn is_mixed(&self) -> bool {
        matches!(self.verdict, Verdict::Mixed { .. }) || matches!(self.year_first, YearFirstVerdict::Mixed { .. })
    }
}

/// Whether the column menu should offer the conversion for `value`: a date
/// that isn't already ISO, or a value shaped like a date that can't be one
/// (the dialog then says why). Cheap, so the menu stops at the first.
pub fn needs_conversion(value: &str) -> bool {
    match classify(value) {
        Some((Parsed::YearFirst { y, p, q, time }, _)) => {
            render(y, p, q, &time).as_deref() != Some(value)
        }
        Some((p, _)) => render_parsed(&p, Plan { dmy: Some(Order::DayFirst), ydm: false }).as_deref() != Some(value),
        None => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Ordered,   // d/m/y or m/d/y
    YearFirst, // 2026/9/25, 2026-09-25
    Named,     // 25-Apr-2026, Apr 25, 2026
}

#[derive(Debug)]
enum Parsed {
    Ready { y: i32, m: u32, d: u32, time: Option<String> },
    NeedsOrder { a: u32, b: u32, y: i32, time: Option<String> },
    /// Year first, then parts `p` and `q`: month-day, or day-month when proven.
    YearFirst { y: i32, p: u32, q: u32, time: Option<String> },
}

#[derive(Debug, Clone, Copy)]
enum Tok<'a> {
    Num(&'a str),
    Word(&'a str),
}

/// The value as ISO 8601, when it is a date this can read under `plan` and
/// the result differs from it.
pub fn to_iso(value: &str, plan: Plan) -> Option<String> {
    let (parsed, _) = classify(value)?;
    let out = render_parsed(&parsed, plan)?;
    (out != value).then_some(out)
}

/// Read a column's values (nulls excluded) and decide how to convert them.
pub fn analyze(values: &[&str]) -> Analysis {
    let (mut dayfirst, mut monthfirst, mut ydm) = (0usize, 0usize, 0usize);
    let (mut dayfirst_ex, mut monthfirst_ex, mut ambiguous_ex) = (None, None, None);
    let (mut ymd_ex, mut ydm_ex) = (None, None);
    for v in values {
        match classify(v) {
            Some((Parsed::NeedsOrder { a, b, .. }, _)) => {
                if a > 12 && b <= 12 {
                    dayfirst += 1;
                    dayfirst_ex.get_or_insert_with(|| v.to_string());
                } else if b > 12 && a <= 12 {
                    monthfirst += 1;
                    monthfirst_ex.get_or_insert_with(|| v.to_string());
                } else if a <= 12 && b <= 12 {
                    ambiguous_ex.get_or_insert_with(|| v.to_string());
                }
            }
            Some((Parsed::YearFirst { p, q, .. }, _)) => {
                if p > 12 && q <= 12 {
                    ydm += 1;
                    ydm_ex.get_or_insert_with(|| v.to_string());
                } else if q > 12 && p <= 12 {
                    ymd_ex.get_or_insert_with(|| v.to_string());
                }
            }
            _ => {}
        }
    }
    let verdict = match (dayfirst_ex, monthfirst_ex) {
        (Some(d), Some(m)) => Verdict::Mixed { dayfirst: d, monthfirst: m },
        (Some(d), None) => Verdict::Proven { order: Order::DayFirst, evidence: d, count: dayfirst },
        (None, Some(m)) => Verdict::Proven { order: Order::MonthFirst, evidence: m, count: monthfirst },
        (None, None) => match ambiguous_ex {
            Some(example) => Verdict::Ambiguous { example },
            None => Verdict::NoQuestion,
        },
    };
    let year_first = match (ymd_ex, ydm_ex) {
        (Some(ymd), Some(ydm)) => YearFirstVerdict::Mixed { ymd, ydm },
        (None, Some(evidence)) => YearFirstVerdict::Ydm { evidence, count: ydm },
        _ => YearFirstVerdict::Ymd,
    };
    // Count with the plan the conversion will use. An ambiguous column's
    // d/m/y values all have both parts <= 12, so either order converts the
    // same ones; day-first stands in.
    let plan = Plan {
        dmy: match &verdict {
            Verdict::Proven { order, .. } => Some(*order),
            Verdict::Ambiguous { .. } => Some(Order::DayFirst),
            _ => None,
        },
        ydm: matches!(year_first, YearFirstVerdict::Ydm { .. }),
    };
    let mut analysis = Analysis {
        verdict,
        year_first,
        convert: 0,
        already: 0,
        impossible: 0,
        impossible_example: None,
        left: 0,
        left_example: None,
        examples: Vec::new(),
    };
    let mut shapes_seen: Vec<Shape> = Vec::new();
    for v in values {
        if v.trim().is_empty() {
            continue;
        }
        let Some((parsed, shape)) = classify(v) else {
            analysis.left += 1;
            analysis.left_example.get_or_insert_with(|| v.to_string());
            continue;
        };
        match render_parsed(&parsed, plan) {
            Some(out) if out != *v => {
                analysis.convert += 1;
                if analysis.examples.len() < 3 && !shapes_seen.contains(&shape) {
                    shapes_seen.push(shape);
                    analysis.examples.push(v.to_string());
                }
            }
            Some(_) => analysis.already += 1,
            None => {
                analysis.impossible += 1;
                analysis.impossible_example.get_or_insert_with(|| v.to_string());
            }
        }
    }
    analysis
}

fn render_parsed(parsed: &Parsed, plan: Plan) -> Option<String> {
    match parsed {
        Parsed::Ready { y, m, d, time } => render(*y, *m, *d, time),
        Parsed::NeedsOrder { a, b, y, time } => {
            let (d, m) = match plan.dmy? {
                Order::DayFirst => (*a, *b),
                Order::MonthFirst => (*b, *a),
            };
            render(*y, m, d, time)
        }
        Parsed::YearFirst { y, p, q, time } => {
            let (m, d) = if plan.ydm { (*q, *p) } else { (*p, *q) };
            render(*y, m, d, time)
        }
    }
}

fn render(y: i32, m: u32, d: u32, time: &Option<String>) -> Option<String> {
    if !(1..=9999).contains(&y) || !(1..=12).contains(&m) || d == 0 || d > days_in_month(y, m) {
        return None;
    }
    Some(match time {
        Some(t) => format!("{y:04}-{m:02}-{d:02} {t}"),
        None => format!("{y:04}-{m:02}-{d:02}"),
    })
}

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        4 | 6 | 9 | 11 => 30,
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        _ => 31,
    }
}

fn two_digit_year(y: u32) -> i32 {
    if y < 70 { 2000 + y as i32 } else { 1900 + y as i32 }
}

fn year(s: &str) -> Option<i32> {
    let n: u32 = s.parse().ok()?;
    match s.len() {
        2 => Some(two_digit_year(n)),
        4 => Some(n as i32),
        _ => None,
    }
}

fn month_number(word: &str) -> Option<u32> {
    const MONTHS: [&str; 12] = [
        "january", "february", "march", "april", "may", "june",
        "july", "august", "september", "october", "november", "december",
    ];
    let w = word.to_ascii_lowercase();
    if w == "sept" {
        return Some(9);
    }
    MONTHS
        .iter()
        .position(|m| *m == w || (w.len() == 3 && m.starts_with(&w)))
        .map(|i| i as u32 + 1)
}

/// Recognise a date at the start of `value` (after trimming) and its shape.
fn classify(value: &str) -> Option<(Parsed, Shape)> {
    let s = value.trim();
    let (toks, seps, rest) = date_tokens(s)?;
    let time = parse_tail(rest)?;
    let numeric_seps = seps.iter().all(|sep| matches!(*sep, "/" | "-" | "."));
    let short = |t: &str| (1..=2).contains(&t.len());
    let num = |t: &str| t.parse::<u32>().ok();
    match toks {
        [Tok::Num(y), Tok::Num(p), Tok::Num(q)] if y.len() == 4 && short(p) && short(q) && numeric_seps => Some((
            Parsed::YearFirst { y: year(y)?, p: num(p)?, q: num(q)?, time },
            Shape::YearFirst,
        )),
        [Tok::Num(a), Tok::Num(b), Tok::Num(y)] if short(a) && short(b) && numeric_seps => Some((
            Parsed::NeedsOrder { a: num(a)?, b: num(b)?, y: year(y)?, time },
            Shape::Ordered,
        )),
        [Tok::Num(y), Tok::Word(mon), Tok::Num(d)] if y.len() == 4 && short(d) => Some((
            Parsed::Ready { y: year(y)?, m: month_number(mon)?, d: num(d)?, time },
            Shape::Named,
        )),
        [Tok::Num(d), Tok::Word(mon), Tok::Num(y)] if short(d) => Some((
            Parsed::Ready { y: year(y)?, m: month_number(mon)?, d: num(d)?, time },
            Shape::Named,
        )),
        [Tok::Word(mon), Tok::Num(d), Tok::Num(y)] if short(d) => Some((
            Parsed::Ready { y: year(y)?, m: month_number(mon)?, d: num(d)?, time },
            Shape::Named,
        )),
        _ => None,
    }
}

/// The first three tokens (digit runs or letter runs) of `s`, the separators
/// between them (any mix of space - . / , for named months), and the rest.
fn date_tokens(s: &str) -> Option<([Tok<'_>; 3], [&str; 2], &str)> {
    let b = s.as_bytes();
    let mut i = 0;
    let mut toks: Vec<Tok> = Vec::with_capacity(3);
    let mut seps: Vec<&str> = Vec::with_capacity(2);
    while toks.len() < 3 {
        if !toks.is_empty() {
            let start = i;
            while i < b.len() && matches!(b[i], b' ' | b'-' | b'.' | b'/' | b',') {
                i += 1;
            }
            seps.push(&s[start..i]);
        }
        let start = i;
        if i < b.len() && b[i].is_ascii_digit() {
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            toks.push(Tok::Num(&s[start..i]));
        } else if i < b.len() && b[i].is_ascii_alphabetic() {
            while i < b.len() && b[i].is_ascii_alphabetic() {
                i += 1;
            }
            toks.push(Tok::Word(&s[start..i]));
        } else {
            return None;
        }
    }
    Some(([toks[0], toks[1], toks[2]], [seps[0], seps[1]], &s[i..]))
}

/// What follows the date: nothing (`Some(None)`), or a space / `T` and a time
/// (`Some(Some(normalised))`). Anything else means the value isn't a clean date.
fn parse_tail(rest: &str) -> Option<Option<String>> {
    if rest.is_empty() {
        return Some(None);
    }
    let time = if let Some(t) = rest.strip_prefix(['T', 't']) {
        t
    } else if rest.starts_with(' ') {
        rest.trim_start()
    } else {
        return None;
    };
    normalize_time(time).map(Some)
}

/// `H:MM[:SS[.fff]]` with an optional am/pm and timezone, as 24-hour
/// `HH:MM:SS[.fff][Z|±HH:MM]`.
fn normalize_time(t: &str) -> Option<String> {
    let b = t.as_bytes();
    let mut i = 0;
    let digits = |i: &mut usize| -> &str {
        let start = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        &t[start..*i]
    };
    let hour_s = digits(&mut i);
    if !(1..=2).contains(&hour_s.len()) || b.get(i) != Some(&b':') {
        return None;
    }
    i += 1;
    let min_s = digits(&mut i);
    if min_s.len() != 2 {
        return None;
    }
    let mut sec: u32 = 0;
    let mut frac = "";
    if b.get(i) == Some(&b':') {
        i += 1;
        let sec_s = digits(&mut i);
        if sec_s.len() != 2 {
            return None;
        }
        sec = sec_s.parse().ok()?;
        if matches!(b.get(i), Some(b'.') | Some(b',')) {
            i += 1;
            frac = digits(&mut i);
            if frac.is_empty() {
                return None;
            }
        }
    }
    let (mut hour, min): (u32, u32) = (hour_s.parse().ok()?, min_s.parse().ok()?);

    // am / pm (also a.m. / p.m.)
    let mut rest = t[i..].trim_start();
    let lower = rest.to_ascii_lowercase();
    let meridiem = ["a.m.", "p.m.", "am", "pm"]
        .iter()
        .find(|m| lower.starts_with(**m))
        .copied();
    if let Some(m) = meridiem {
        rest = rest[m.len()..].trim_start();
        let pm = m.starts_with('p');
        hour = match (hour, pm) {
            (12, false) => 0,
            (12, true) => 12,
            (h @ 0..=11, false) => h,
            (h @ 1..=11, true) => h + 12,
            _ => return None,
        };
    }
    if hour > 23 || min > 59 || sec > 59 {
        return None;
    }

    // Timezone: Z, or ±HH[:MM] / ±HHMM
    let tz = if rest.is_empty() {
        String::new()
    } else if rest.eq_ignore_ascii_case("z") {
        "Z".to_string()
    } else {
        let sign = rest.chars().next()?;
        if sign != '+' && sign != '-' {
            return None;
        }
        let body = rest[1..].replace(':', "");
        if !body.bytes().all(|c| c.is_ascii_digit()) || !(body.len() == 2 || body.len() == 4) {
            return None;
        }
        let oh: u32 = body[..2].parse().ok()?;
        let om: u32 = if body.len() == 4 { body[2..].parse().ok()? } else { 0 };
        if oh > 23 || om > 59 {
            return None;
        }
        format!("{sign}{oh:02}:{om:02}")
    };
    let frac = if frac.is_empty() { String::new() } else { format!(".{frac}") };
    Some(format!("{hour:02}:{min:02}:{sec:02}{frac}{tz}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iso(v: &str, dmy: Option<Order>) -> Option<String> {
        to_iso(v, Plan { dmy, ydm: false })
    }

    const DAY: Option<Order> = Some(Order::DayFirst);
    const MONTH: Option<Order> = Some(Order::MonthFirst);

    #[test]
    fn day_and_month_first_dates_with_times() {
        assert_eq!(iso("25/04/26", DAY).as_deref(), Some("2026-04-25"));
        assert_eq!(iso("03-25-2026 02:15 PM", MONTH).as_deref(), Some("2026-03-25 14:15:00"));
        assert_eq!(iso("1.5.2026 9:07", DAY).as_deref(), Some("2026-05-01 09:07:00"));
        assert_eq!(iso("01/15/2026 12:05 AM", MONTH).as_deref(), Some("2026-01-15 00:05:00"));
        assert_eq!(iso("01/15/2026 12:30 p.m.", MONTH).as_deref(), Some("2026-01-15 12:30:00"));
        assert_eq!(iso("15/01/2026 23:59:59,5", DAY).as_deref(), Some("2026-01-15 23:59:59.5"));
        // d/m/y needs an order; without one it is left alone.
        assert_eq!(iso("03/04/2026", None), None);
    }

    #[test]
    fn named_months_and_year_first_need_no_order() {
        for (v, want) in [
            ("25-Apr-2026", "2026-04-25"),
            ("Apr 25, 2026", "2026-04-25"),
            ("September 25 2026", "2026-09-25"),
            ("25Apr26", "2026-04-25"),
            ("2026-Apr-25", "2026-04-25"),
            ("sept 5 2026", "2026-09-05"),
            ("2026/9/5", "2026-09-05"),
            ("2026.09.25 7:00 am", "2026-09-25 07:00:00"),
        ] {
            assert_eq!(iso(v, None).as_deref(), Some(want), "{v}");
        }
    }

    #[test]
    fn iso_values_are_normalised_or_left_alone() {
        assert_eq!(iso("2026-09-25", None), None);
        assert_eq!(iso("2026-09-25 10:00:00", None), None);
        assert_eq!(iso("2026-09-25T10:00", None).as_deref(), Some("2026-09-25 10:00:00"));
        assert_eq!(iso("2026-09-25 10:00:00.123 -0700", None).as_deref(), Some("2026-09-25 10:00:00.123-07:00"));
        assert_eq!(iso("2026-09-25T10:00:00Z", None).as_deref(), Some("2026-09-25 10:00:00Z"));
        assert_eq!(iso(" 2026-09-25 ", None).as_deref(), Some("2026-09-25"));
    }

    #[test]
    fn two_digit_years_pivot_at_70() {
        assert_eq!(iso("01/02/69", DAY).as_deref(), Some("2069-02-01"));
        assert_eq!(iso("01/02/70", DAY).as_deref(), Some("1970-02-01"));
    }

    #[test]
    fn impossible_dates_and_non_dates_are_left_alone() {
        for v in [
            "31/02/2026", "2026-13-01", "2026-02-29", "13:00 PM", "25/04/2026 13:00 PM",
            "25/04/2026 (est)", "45678", "1.2.3", "192.168.1.10", "TBD", "12.5",
            "403-555-1234", "March 2026", "Tue 25 Sep 2026", "25/04/2026 9:5",
        ] {
            assert_eq!(iso(v, DAY), None, "{v}");
            assert_eq!(iso(v, MONTH), None, "{v}");
        }
        assert_eq!(iso("2024-02-29", None), None); // valid leap day, already ISO
        assert_eq!(iso("29/02/2024", DAY).as_deref(), Some("2024-02-29"));
    }

    #[test]
    fn verdicts_follow_abp() {
        let a = analyze(&["03/04/2026", "25/04/2026", "TBD", "2026-01-01", ""]);
        assert!(matches!(a.verdict, Verdict::Proven { order: Order::DayFirst, .. }));
        assert_eq!((a.convert, a.already, a.left), (2, 1, 1));
        assert_eq!(a.left_example.as_deref(), Some("TBD"));

        let a = analyze(&["03/25/2026 02:15 PM", "04/01/2026 09:00 AM"]);
        assert!(matches!(a.verdict, Verdict::Proven { order: Order::MonthFirst, count: 1, .. }));
        assert_eq!(a.convert, 2);

        let a = analyze(&["25/04/2026", "04/25/2026"]);
        assert!(matches!(a.verdict, Verdict::Mixed { .. }));

        let a = analyze(&["03/04/2026", "05/06/2026"]);
        assert_eq!(a.verdict, Verdict::Ambiguous { example: "03/04/2026".to_string() });
        assert_eq!(a.convert, 2);

        let a = analyze(&["25-Apr-2026", "2026-09-25"]);
        assert_eq!(a.verdict, Verdict::NoQuestion);
        assert_eq!((a.convert, a.already), (1, 1));

        let a = analyze(&["alpha", "beta", "2026-09-25"]);
        assert_eq!(a.convert, 0);
    }

    #[test]
    fn needs_conversion_flags_dates_to_rewrite_and_impossible_ones() {
        for v in [
            "25/04/2026", "03/04/2026", "Apr 5 2026", "2026-09-25T10:00", "2026/9/5",
            "2023-31-12", "20/20/2000", "2026-02-30",
        ] {
            assert!(needs_conversion(v), "{v}");
        }
        for v in ["2026-09-25", "2026-09-25 10:00:00", "TBD", "45678", ""] {
            assert!(!needs_conversion(v), "{v}");
        }
    }

    #[test]
    fn the_three_column_test_file() {
        // 2023-31-12: year first, then a middle part over 12 proves year-day-month.
        let a = analyze(&["2023-31-12"]);
        assert!(matches!(a.year_first, YearFirstVerdict::Ydm { count: 1, .. }));
        assert_eq!(a.convert, 1);
        assert_eq!(to_iso("2023-31-12", Plan { dmy: None, ydm: true }).as_deref(), Some("2023-12-31"));
        assert_eq!(to_iso("2023-31-12", Plan::default()), None); // never without the proof
        // 11-may-13: a named month.
        assert_eq!(to_iso("11-may-13", Plan::default()).as_deref(), Some("2013-05-11"));
        // 20/20/2000: no month 20 in either reading; reported, left alone.
        let a = analyze(&["20/20/2000"]);
        assert_eq!((a.convert, a.impossible), (0, 1));
        assert_eq!(a.impossible_example.as_deref(), Some("20/20/2000"));
    }

    #[test]
    fn year_day_month_is_proven_per_column_and_refused_when_mixed() {
        let a = analyze(&["2023-31-12", "2023-05-06"]);
        assert!(matches!(a.year_first, YearFirstVerdict::Ydm { .. }));
        assert_eq!(a.convert, 2); // 2023-05-06 is read day-month here too
        assert_eq!(to_iso("2023-05-06", Plan { dmy: None, ydm: true }).as_deref(), Some("2023-06-05"));

        let a = analyze(&["2023-31-12", "2023-12-31"]);
        assert!(matches!(a.year_first, YearFirstVerdict::Mixed { .. }));
        assert!(a.is_mixed());

        // Plain ISO needs no proof and is never re-read.
        let a = analyze(&["2023-12-31", "2023-05-06"]);
        assert_eq!(a.year_first, YearFirstVerdict::Ymd);
        assert_eq!((a.convert, a.already), (0, 2));
    }

    #[test]
    fn examples_are_one_per_format() {
        let a = analyze(&["03/25/2026", "04/25/2026", "25-Apr-2026", "2026/9/5", "Apr 1, 2026"]);
        assert_eq!(a.examples, vec!["03/25/2026", "25-Apr-2026", "2026/9/5"]);
    }
}
