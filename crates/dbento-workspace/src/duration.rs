//! The human duration syntax of `statement_timeout` and `--timeout`: `30s`, `1m 30s`, `2.5h`.
//!
//! Parsing accepts exactly the strings humantime 2.x accepted, with the same error messages, and
//! formatting prints the same text, so existing workspaces and scripts keep working. Months are
//! 30.44 days and years 365.25 days, in both directions.

use std::fmt;
use std::time::Duration;

const NANOS_PER_SEC: u64 = 1_000_000_000;
const SECS_PER_MONTH: u64 = 2_630_016;
const SECS_PER_YEAR: u64 = 31_557_600;

/// Why a duration string did not parse. Offsets are byte offsets into the input.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DurationError {
    /// The input was empty or only whitespace.
    #[error("value was empty")]
    Empty,
    /// A character that belongs in neither a number nor a unit.
    #[error("invalid character at {offset}")]
    InvalidCharacter {
        /// Byte offset of the offending character.
        offset: usize,
    },
    /// A part did not start with a digit, as in `2 hours min`.
    #[error("expected number at {offset}")]
    NumberExpected {
        /// Byte offset where the number was expected.
        offset: usize,
    },
    /// A number with no unit, or a unit that is not one of the supported names.
    #[error(fmt = unknown_unit_message)]
    UnknownUnit {
        /// The unit as written; empty when the number had none.
        unit: String,
        /// The whole-number part the unit belonged to.
        value: u64,
    },
    /// The value does not fit a `Duration`, or needs precision below one nanosecond.
    #[error(
        "number is too large or cannot be represented without a lack of precision (values below 1ns are not supported)"
    )]
    NumberOverflow,
}

fn unknown_unit_message(unit: &str, value: &u64, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    if unit.is_empty() {
        write!(f, "time unit needed, for example {value}sec or {value}ms")
    } else {
        write!(
            f,
            "unknown time unit {unit:?}, supported units: ns, us/µs, ms, sec, min, hours, days, \
             weeks, months, years (and few variations)"
        )
    }
}

/// A duration displayed in the syntax `parse_duration` reads back, such as `1h 2m 3s`.
#[derive(Debug, Clone, Copy)]
pub struct FormattedDuration(Duration);

/// Wraps `d` so that its `Display` prints the human form, largest unit first.
pub fn format_duration(d: Duration) -> FormattedDuration {
    FormattedDuration(d)
}

impl fmt::Display for FormattedDuration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_zero() {
            return f.write_str("0s");
        }
        // Calendar parts take a plural word, clock parts a fixed suffix; zero parts are left out.
        let secs = self.0.as_secs();
        let nanos = u64::from(self.0.subsec_nanos());
        let in_year = secs % SECS_PER_YEAR;
        let in_month = in_year % SECS_PER_MONTH;
        let in_day = in_month % 86_400;
        let parts: [(u64, &str, bool); 9] = [
            (secs / SECS_PER_YEAR, "year", true),
            (in_year / SECS_PER_MONTH, "month", true),
            (in_month / 86_400, "day", true),
            (in_day / 3600, "h", false),
            (in_day % 3600 / 60, "m", false),
            (in_day % 60, "s", false),
            (nanos / 1_000_000, "ms", false),
            (nanos / 1000 % 1000, "us", false),
            (nanos % 1000, "ns", false),
        ];
        let mut separator = "";
        for (value, name, plural) in parts.into_iter().filter(|p| p.0 > 0) {
            let suffix = if plural && value > 1 { "s" } else { "" };
            write!(f, "{separator}{value}{name}{suffix}")?;
            separator = " ";
        }
        Ok(())
    }
}

/// The units a part may carry, matched case-sensitively (`m` is minutes, `M` months).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unit {
    Nano,
    Micro,
    Milli,
    Second,
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Year,
}

impl Unit {
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "nanos" | "nsec" | "ns" => Self::Nano,
            "usec" | "us" | "\u{b5}s" => Self::Micro,
            "millis" | "msec" | "ms" => Self::Milli,
            "seconds" | "second" | "secs" | "sec" | "s" => Self::Second,
            "minutes" | "minute" | "min" | "mins" | "m" => Self::Minute,
            "hours" | "hour" | "hr" | "hrs" | "h" => Self::Hour,
            "days" | "day" | "d" => Self::Day,
            "weeks" | "week" | "wk" | "wks" | "w" => Self::Week,
            "months" | "month" | "M" => Self::Month,
            "years" | "year" | "yr" | "yrs" | "y" => Self::Year,
            _ => return None,
        })
    }

    /// The length of one unit as (seconds, nanoseconds); exactly one of the two is non-zero.
    fn length(self) -> (u64, u64) {
        match self {
            Self::Nano => (0, 1),
            Self::Micro => (0, 1000),
            Self::Milli => (0, 1_000_000),
            Self::Second => (1, 0),
            Self::Minute => (60, 0),
            Self::Hour => (3600, 0),
            Self::Day => (86_400, 0),
            Self::Week => (7 * 86_400, 0),
            Self::Month => (SECS_PER_MONTH, 0),
            Self::Year => (SECS_PER_YEAR, 0),
        }
    }

    /// The (seconds, nanoseconds) a decimal fraction `numerator / denominator` of this unit adds.
    /// Up to a minute the fraction must land on a whole nanosecond, above it on a whole second.
    fn fraction(self, numerator: u64, denominator: u64) -> Result<(u64, u64), DurationError> {
        let exact = |scale: u64| {
            let scaled = checked(numerator.checked_mul(scale))?;
            if scaled % denominator == 0 {
                Ok(scaled / denominator)
            } else {
                Err(DurationError::NumberOverflow)
            }
        };
        match self {
            Self::Nano => Err(DurationError::NumberOverflow),
            Self::Micro => Ok((0, exact(1000)?)),
            Self::Milli => Ok((0, exact(1_000_000)?)),
            Self::Second => Ok((0, exact(NANOS_PER_SEC)?)),
            Self::Minute => Ok((0, exact(60 * NANOS_PER_SEC)?)),
            Self::Hour | Self::Day | Self::Week | Self::Month | Self::Year => {
                Ok((exact(self.length().0)?, 0))
            }
        }
    }
}

fn checked(value: Option<u64>) -> Result<u64, DurationError> {
    value.ok_or(DurationError::NumberOverflow)
}

fn is_unit_char(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '\u{b5}'
}

/// A byte cursor over the input that hands out characters with their offsets.
struct Cursor<'a> {
    src: &'a str,
    pos: usize,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<char> {
        self.src
            .get(self.pos..)
            .and_then(|rest| rest.chars().next())
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }
}

/// The running total, kept normalised so `nanos` stays below one second.
#[derive(Default)]
struct Total {
    secs: u64,
    nanos: u64,
}

impl Total {
    fn add(&mut self, secs: u64, nanos: u64) -> Result<(), DurationError> {
        let nanos = checked(self.nanos.checked_add(nanos))?;
        let carried = checked(secs.checked_add(nanos / NANOS_PER_SEC))?;
        self.secs = checked(self.secs.checked_add(carried))?;
        self.nanos = nanos % NANOS_PER_SEC;
        Ok(())
    }
}

/// Parses a duration such as `30s`, `1m 30s`, `1h\t2m` or `2.5 h`; a bare `0` is zero.
///
/// Fails with the variant that names the first problem, at the byte offset it was found.
pub fn parse_duration(s: &str) -> Result<Duration, DurationError> {
    // Read parts of `digits [. digits] unit` left to right. Whitespace may sit anywhere inside the
    // number and is skipped; a unit runs over letters and ends at whitespace or at the digit that
    // starts the next part. Each part is converted and added at once, all with checked arithmetic.
    if s == "0" {
        return Ok(Duration::ZERO);
    }
    let mut cur = Cursor { src: s, pos: 0 };
    let mut total = Total::default();
    let mut next_digit = None;
    let mut first = true;
    loop {
        let lead = match next_digit.take() {
            Some(d) => d,
            None => match first_digit(&mut cur)? {
                Some(d) => d,
                None if first => return Err(DurationError::Empty),
                None => break,
            },
        };
        first = false;
        let (whole, fraction) = read_number(&mut cur, lead)?;
        let (name, digit) = read_unit(&mut cur)?;
        next_digit = digit;
        let unit = Unit::from_name(name).ok_or_else(|| DurationError::UnknownUnit {
            unit: name.to_owned(),
            value: whole,
        })?;
        let (secs, nanos) = unit.length();
        total.add(
            checked(whole.checked_mul(secs))?,
            checked(whole.checked_mul(nanos))?,
        )?;
        if let Some((numerator, denominator)) = fraction {
            let (secs, nanos) = unit.fraction(numerator, denominator)?;
            total.add(secs, nanos)?;
        }
    }
    let nanos = u32::try_from(total.nanos).map_err(|_| DurationError::NumberOverflow)?;
    Ok(Duration::new(total.secs, nanos))
}

/// Skips whitespace to the first digit of a part. `None` means the input ended; anything else
/// is reported at the offset where the skipping began.
fn first_digit(cur: &mut Cursor<'_>) -> Result<Option<u64>, DurationError> {
    let start = cur.pos;
    while let Some(c) = cur.bump() {
        if let Some(d) = c.to_digit(10) {
            return Ok(Some(u64::from(d)));
        }
        if !c.is_whitespace() {
            return Err(DurationError::NumberExpected { offset: start });
        }
    }
    Ok(None)
}

/// Reads the rest of a number after its first digit, stopping before the unit's first letter.
/// Returns the whole part and, after a `.`, the fraction as (numerator, power of ten).
fn read_number(
    cur: &mut Cursor<'_>,
    lead: u64,
) -> Result<(u64, Option<(u64, u64)>), DurationError> {
    let mut whole = lead;
    while let Some(c) = cur.peek() {
        if is_unit_char(c) {
            return Ok((whole, None));
        }
        let at = cur.pos;
        cur.bump();
        if let Some(d) = c.to_digit(10) {
            whole = checked(
                whole
                    .checked_mul(10)
                    .and_then(|w| w.checked_add(u64::from(d))),
            )?;
        } else if c == '.' {
            return Ok((whole, Some(read_fraction(cur, at)?)));
        } else if !c.is_whitespace() {
            return Err(DurationError::InvalidCharacter { offset: at });
        }
    }
    Ok((whole, None))
}

/// Reads the digits after a `.` at `dot`. An error points at the last position the fraction
/// reached, which is the dot itself until a digit or space has been read.
fn read_fraction(cur: &mut Cursor<'_>, dot: usize) -> Result<(u64, u64), DurationError> {
    let mut numerator = 0u64;
    let mut denominator = 1u64;
    let mut reached = dot;
    while let Some(c) = cur.peek() {
        if is_unit_char(c) {
            break;
        }
        if let Some(d) = c.to_digit(10) {
            denominator = checked(denominator.checked_mul(10))?;
            numerator = checked(
                numerator
                    .checked_mul(10)
                    .and_then(|n| n.checked_add(u64::from(d))),
            )?;
        } else if !c.is_whitespace() {
            return Err(DurationError::InvalidCharacter { offset: reached });
        }
        cur.bump();
        reached = cur.pos;
    }
    if denominator == 1 {
        return Err(DurationError::InvalidCharacter { offset: reached });
    }
    Ok((numerator, denominator))
}

/// Reads a unit name, consuming one whitespace character that ends it. A digit that ends it is
/// consumed too and returned, as it is the first digit of the next part.
fn read_unit<'a>(cur: &mut Cursor<'a>) -> Result<(&'a str, Option<u64>), DurationError> {
    let src = cur.src;
    let start = cur.pos;
    while let Some(c) = cur.peek() {
        let end = cur.pos;
        if is_unit_char(c) {
            cur.bump();
            continue;
        }
        let name = src.get(start..end).unwrap_or_default();
        if let Some(d) = c.to_digit(10) {
            cur.bump();
            return Ok((name, Some(u64::from(d))));
        }
        if c.is_whitespace() {
            cur.bump();
            return Ok((name, None));
        }
        return Err(DurationError::InvalidCharacter { offset: end });
    }
    Ok((src.get(start..).unwrap_or_default(), None))
}
