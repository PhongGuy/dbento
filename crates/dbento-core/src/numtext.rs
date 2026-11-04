//! Number and calendar text shared by the DuckDB and SQL Server renderers: floats in
//! DuckDB's shortest round-trip form, exact decimals with every digit of their scale, and
//! proleptic Gregorian dates from a day count.

use std::fmt::Write as _;

/// A float as DuckDB prints it: shortest round-trip digits, positional between 1e-4 and
/// 1e16 with at least one fraction digit, otherwise `1.5e+20` with a two-digit exponent.
pub(crate) fn float_text(
    s: &mut String,
    nan: bool,
    inf: bool,
    negative: bool,
    sci: std::fmt::Arguments,
) {
    if nan {
        s.push_str("nan");
        return;
    }
    if inf {
        s.push_str(if negative { "-inf" } else { "inf" });
        return;
    }
    // `{:e}` gives the shortest digits as `-d.ddde-x`; split it into sign, digits and exponent.
    let mut buf = String::with_capacity(32);
    _ = buf.write_fmt(sci);
    let (mantissa, exp) = buf.split_once('e').unwrap_or((&buf, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(m) => ("-", m),
        None => ("", mantissa),
    };
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    s.push_str(sign);
    if !(-4..16).contains(&exp) {
        s.push_str(&digits[..1]);
        if digits.len() > 1 {
            s.push('.');
            s.push_str(&digits[1..]);
        }
        _ = write!(
            s,
            "e{}{:02}",
            if exp < 0 { '-' } else { '+' },
            exp.unsigned_abs()
        );
    } else if exp >= 0 {
        let int_len = exp as usize + 1;
        if digits.len() > int_len {
            s.push_str(&digits[..int_len]);
            s.push('.');
            s.push_str(&digits[int_len..]);
        } else {
            s.push_str(&digits);
            s.extend(std::iter::repeat_n('0', int_len - digits.len()));
            s.push_str(".0");
        }
    } else {
        s.push_str("0.");
        s.extend(std::iter::repeat_n('0', (-exp - 1) as usize));
        s.push_str(&digits);
    }
}

/// An exact decimal from its unscaled value: `-0.05`, `0.00`, `1.5`.
pub(crate) fn decimal_text(s: &mut String, unscaled: i128, scale: i8) {
    let scale = scale.max(0) as u32;
    if unscaled < 0 {
        s.push('-');
    }
    let magnitude = unscaled.unsigned_abs();
    let ten = 10u128.pow(scale);
    _ = write!(s, "{}", magnitude / ten);
    if scale > 0 {
        _ = write!(s, ".{:0width$}", magnitude % ten, width = scale as usize);
    }
}

/// Days since 1970-01-01 as a proleptic Gregorian (year, month, day); year 0 is 1 BC.
pub(crate) fn civil(days: i64) -> (i64, u32, u32) {
    // Howard Hinnant's days-to-civil algorithm, shifted to eras starting 0000-03-01.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f64_text(v: f64) -> String {
        let mut s = String::new();
        float_text(
            &mut s,
            v.is_nan(),
            v.is_infinite(),
            v.is_sign_negative(),
            format_args!("{v:e}"),
        );
        s
    }

    #[test]
    fn floats_print_their_shortest_digits_positionally_or_with_a_signed_exponent() {
        let cases = [
            (0.1, "0.1"),
            (1e20, "1e+20"),
            (0.0, "0.0"),
            (1e-5, "1e-05"),
            (123456789012.0, "123456789012.0"),
            (-1.7976931348623157e308, "-1.7976931348623157e+308"),
            (2.2250738585072014e-308, "2.2250738585072014e-308"),
        ];
        for (v, want) in cases {
            assert_eq!(f64_text(v), want, "{v:e}");
        }
        let mut s = String::new();
        let r = f32::MIN_POSITIVE;
        float_text(&mut s, false, false, false, format_args!("{r:e}"));
        assert_eq!(s, "1.1754944e-38");
    }

    #[test]
    fn decimals_keep_every_digit_of_their_scale() {
        let text = |unscaled: i128, scale: i8| {
            let mut s = String::new();
            decimal_text(&mut s, unscaled, scale);
            s
        };
        assert_eq!(text(-5, 2), "-0.05");
        assert_eq!(text(0, 2), "0.00");
        assert_eq!(text(15, 1), "1.5");
        assert_eq!(text(12, 0), "12");
        assert_eq!(text(10_000, 4), "1.0000");
    }

    #[test]
    fn civil_dates_cover_years_one_to_9999() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(-719_162), (1, 1, 1));
        assert_eq!(civil(2_932_896), (9999, 12, 31));
        assert_eq!(civil(19_782), (2024, 2, 29));
    }
}
