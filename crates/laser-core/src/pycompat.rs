use num_bigint::BigUint;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn py_round(value: f64, ndigits: i32) -> f64 {
    if !value.is_finite() || value == 0.0 || ndigits > 323 {
        return value;
    }
    if ndigits < -308 {
        return 0.0_f64.copysign(value);
    }
    if ndigits >= 0 && ndigits <= 308 && value.abs() < 0.5 * 10_f64.powi(-ndigits) {
        return 0.0_f64.copysign(value);
    }
    // Round the exact binary rational, not value * 10^n (which introduces a
    // second rounding before the tie decision, e.g. round(2.675, 2)).
    let bits = value.abs().to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i32;
    let mantissa = (bits & ((1_u64 << 52) - 1)) | if exponent == 0 { 0 } else { 1_u64 << 52 };
    let shift = if exponent == 0 {
        -1074
    } else {
        exponent - 1075
    };
    let mut numerator = BigUint::from(mantissa);
    let mut denominator = BigUint::from(1_u8);
    if shift >= 0 {
        numerator <<= shift as usize;
    } else {
        denominator <<= -shift as usize;
    }
    let decimal_scale = BigUint::from(10_u8).pow(ndigits.unsigned_abs());
    if ndigits >= 0 {
        numerator *= &decimal_scale;
    } else {
        denominator *= &decimal_scale;
    }
    let mut rounded = &numerator / &denominator;
    let twice_remainder = (&numerator % &denominator) * 2_u8;
    if twice_remainder > denominator
        || (twice_remainder == denominator && &rounded % 2_u8 == BigUint::from(1_u8))
    {
        rounded += 1_u8;
    }
    let text = format!("{}e{}", rounded.to_str_radix(10), -ndigits);
    text.parse::<f64>().unwrap_or(f64::INFINITY).copysign(value)
}

pub fn format_float(value: f64) -> String {
    if value.is_nan() {
        return "nan".into();
    }
    if value == f64::INFINITY {
        return "inf".into();
    }
    if value == f64::NEG_INFINITY {
        return "-inf".into();
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0".into()
        } else {
            "0.0".into()
        };
    }
    let mut buffer = ryu::Buffer::new();
    let text = buffer.format(value).to_owned();
    let negative = text.starts_with('-');
    let s = text.trim_start_matches('-');

    let (digits, exponent): (String, i32) = if let Some(index) = s.find('e').or_else(|| s.find('E'))
    {
        let (mantissa, exp_str) = (&s[..index], &s[index + 1..]);
        let exp_val: i32 = exp_str.parse().expect("float exponent");
        let d = mantissa.replace('.', "");
        let d_trimmed = d.trim_end_matches('0');
        let digits = if d_trimmed.is_empty() { "0" } else { d_trimmed }.to_string();
        (digits, exp_val)
    } else {
        let dot = s.find('.').unwrap_or(s.len());
        let d = s.replace('.', "");
        let first_non_zero = d.find(|c: char| c != '0').unwrap_or(0);
        let exp_val = dot as i32 - first_non_zero as i32 - 1;
        let d_trimmed = d[first_non_zero..].trim_end_matches('0');
        let digits = if d_trimmed.is_empty() { "0" } else { d_trimmed }.to_string();
        (digits, exp_val)
    };

    let body = if exponent < -4 || exponent >= 16 {
        let mantissa = if digits.len() == 1 {
            digits
        } else {
            format!("{}.{}", &digits[..1], &digits[1..])
        };
        format!("{mantissa}e{exponent:+03}")
    } else if exponent + 1 <= 0 {
        let leading_zeros = -(exponent + 1) as usize;
        format!("0.{}{}", "0".repeat(leading_zeros), digits)
    } else if (exponent + 1) as usize >= digits.len() {
        let trailing_zeros = (exponent + 1) as usize - digits.len();
        format!("{}{}.0", digits, "0".repeat(trailing_zeros))
    } else {
        let split = (exponent + 1) as usize;
        format!("{}.{}", &digits[..split], &digits[split..])
    };

    if negative { format!("-{body}") } else { body }
}

pub fn uuid4_hex() -> String {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).expect("OS randomness unavailable");
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn unix_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_secs_f64()
}

pub fn unix_iso8601_now() -> String {
    iso8601_from_micros(chrono::Utc::now().timestamp_micros())
        .expect("current UTC time is representable")
}

pub fn iso8601_from_micros(micros: i64) -> Option<String> {
    let date = chrono::DateTime::from_timestamp_micros(micros)?;
    let pattern = if micros.rem_euclid(1_000_000) == 0 {
        "%Y-%m-%dT%H:%M:%S+00:00"
    } else {
        "%Y-%m-%dT%H:%M:%S%.6f+00:00"
    };
    Some(date.format(pattern).to_string())
}

pub fn strip_inline_comment(line: &str) -> &str {
    line.split_once(';').map_or(line, |(code, _)| code).trim()
}

pub fn strip_windows_extended_prefix(path: &str) -> &str {
    path.strip_prefix(r"\\?\").unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_python_bankers_rounding() {
        assert_eq!(py_round(2.5, 0), 2.0);
        assert_eq!(py_round(3.5, 0), 4.0);
        assert_eq!(py_round(1.25, 1), 1.2);
        assert_eq!(py_round(1.35, 1), 1.4);
        assert_eq!(py_round(2.675, 2), 2.67);
        assert_eq!(py_round(250.0, -2), 200.0);
        assert_eq!(py_round(350.0, -2), 400.0);
        assert_eq!(py_round(1.0000000000000004, 15), 1.0);
        assert_eq!(py_round(1e-18, 17), 0.0);
        assert_eq!(py_round(1e-18, 18), 1e-18);
        assert!(py_round(-0.1, 0).is_sign_negative());
    }

    #[test]
    fn preserves_python_float_shape() {
        assert_eq!(format_float(1.0), "1.0");
        assert_eq!(format_float(1.25), "1.25");
        assert_eq!(format_float(1e-4), "0.0001");
        assert_eq!(format_float(1e-5), "1e-05");
        assert_eq!(format_float(-1e-5), "-1e-05");
        assert_eq!(format_float(1e15), "1000000000000000.0");
        assert_eq!(format_float(1e16), "1e+16");
        assert_eq!(format_float(f64::INFINITY), "inf");
    }

    #[test]
    fn strips_comments_and_extended_windows_prefix() {
        assert_eq!(strip_inline_comment("G1 X1 ; note"), "G1 X1");
        assert_eq!(strip_windows_extended_prefix(r"\\?\C:\tmp\a"), r"C:\tmp\a");
    }

    #[test]
    fn creates_uuid4_hex() {
        let id = uuid4_hex();
        assert_eq!(id.len(), 32);
        assert_eq!(&id[12..13], "4");
    }
}
