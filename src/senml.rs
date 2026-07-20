//! RFC 8428 SenML/JSON pack parsing, specialised to what the sma-daemon
//! contract publishes: one header record carrying `bn`/`bt`/`bu`, followed by
//! plain records (`n`, optional `u`, optional `t` offset, `v` or `vs`).
//!
//! Knows nothing about PV — resolution of names/units into snapshot fields
//! lives in [`crate::domain`].
//!
//! Numbers are kept as their raw decimal text (`serde_json`'s
//! `arbitrary_precision`) and scaled with [`scale_decimal`] — an exact digit
//! shift, never `f64` math — mirroring the publisher's exact-decimal
//! serialisation.

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Raw decimal literal, exactly as published (e.g. `"236.40"`).
    Number(String),
    /// String-valued record (`vs`), used for enums like `device/status`.
    Text(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub name: String,
    /// Resolved unit: the record's `u` if present, else the pack's `bu`.
    pub unit: Option<String>,
    /// The record's `t` relative to the pack base time, in seconds.
    pub time_offset: i64,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pack {
    /// The pack's `bt` (unix seconds).
    pub base_time: i64,
    pub records: Vec<Record>,
}

#[derive(Debug)]
pub enum PackError {
    Json(serde_json::Error),
    /// The array had no header record with base fields.
    MissingHeader,
}

impl core::fmt::Display for PackError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PackError::Json(e) => write!(f, "senml pack is not valid JSON: {e}"),
            PackError::MissingHeader => write!(f, "senml pack has no base-time header record"),
        }
    }
}

impl std::error::Error for PackError {}

/// One raw array element; base fields and regular fields may in principle
/// co-exist (RFC 8428), though the contract keeps the header dedicated.
#[derive(Debug, Deserialize)]
struct RawRecord {
    bn: Option<String>,
    bt: Option<serde_json::Number>,
    bu: Option<String>,
    n: Option<String>,
    u: Option<String>,
    t: Option<serde_json::Number>,
    v: Option<serde_json::Number>,
    vs: Option<String>,
}

/// Parses a retained `instantaneous` payload into a [`Pack`].
///
/// Records without a name or without a value (`v`/`vs`) are skipped — the
/// contract never publishes them, and inventing data is worse than dropping.
pub fn parse_pack(payload: &[u8]) -> Result<Pack, PackError> {
    let raw: Vec<RawRecord> = serde_json::from_slice(payload).map_err(PackError::Json)?;

    let mut base_time: Option<i64> = None;
    let mut base_unit: Option<String> = None;
    let mut records = Vec::with_capacity(raw.len());

    for rec in raw {
        // Base fields apply to this and all subsequent records (RFC 8428 §4.1).
        if rec.bn.is_some() || rec.bt.is_some() || rec.bu.is_some() {
            if let Some(bt) = &rec.bt {
                base_time = Some(scale_decimal(bt.as_str(), 0).unwrap_or(0));
            }
            if let Some(bu) = rec.bu {
                base_unit = Some(bu);
            }
        }

        let Some(name) = rec.n else {
            continue;
        };
        let value = match (rec.v, rec.vs) {
            (Some(v), _) => Value::Number(v.as_str().to_owned()),
            (None, Some(vs)) => Value::Text(vs),
            (None, None) => continue,
        };
        records.push(Record {
            name,
            unit: rec.u.or_else(|| base_unit.clone()),
            time_offset: rec
                .t
                .as_ref()
                .and_then(|t| scale_decimal(t.as_str(), 0))
                .unwrap_or(0),
            value,
        });
    }

    match base_time {
        Some(base_time) => Ok(Pack { base_time, records }),
        None => Err(PackError::MissingHeader),
    }
}

/// Scales a plain decimal literal by `10^exp` into an exact `i64`:
/// `scale_decimal("236.40", 3)` → `236400`.
///
/// This is string/integer arithmetic only. Fraction digits beyond `exp` are
/// rounded half-away-from-zero (the publisher's precision never exceeds the
/// Matter unit's, so in practice nothing is dropped). Returns `None` for
/// anything that isn't a plain `[-]digits[.digits]` literal (exponent
/// notation, empty, overflow) — callers skip the reading rather than guess.
pub fn scale_decimal(raw: &str, exp: u32) -> Option<i64> {
    let raw = raw.trim();
    let (negative, digits) = match raw.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, raw.strip_prefix('+').unwrap_or(raw)),
    };

    let (int_part, frac_part) = match digits.split_once('.') {
        Some((i, f)) => (i, f),
        None => (digits, ""),
    };
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }
    if !int_part.bytes().all(|b| b.is_ascii_digit())
        || !frac_part.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }

    let exp = exp as usize;
    let mut magnitude: i64 = 0;
    let mut push = |digit: u8| -> Option<()> {
        magnitude = magnitude
            .checked_mul(10)?
            .checked_add(i64::from(digit - b'0'))?;
        Some(())
    };

    for b in int_part.bytes() {
        push(b)?;
    }
    // Take exactly `exp` fraction digits, zero-padding on the right.
    let mut frac = frac_part.bytes();
    for _ in 0..exp {
        push(frac.next().unwrap_or(b'0'))?;
    }
    // Round half-away-from-zero on the first surplus digit.
    if let Some(next) = frac.next()
        && next >= b'5'
    {
        magnitude = magnitude.checked_add(1)?;
    }

    Some(if negative { -magnitude } else { magnitude })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_contract_examples_exactly() {
        // From mqtt-contract.md's example pack, into Matter milli-units.
        assert_eq!(scale_decimal("2450", 3), Some(2_450_000)); // W → mW
        assert_eq!(scale_decimal("236.40", 3), Some(236_400)); // V → mV
        assert_eq!(scale_decimal("10.370", 3), Some(10_370)); // A → mA
        assert_eq!(scale_decimal("49.98", 3), Some(49_980)); // Hz → mHz
        assert_eq!(scale_decimal("41784321", 3), Some(41_784_321_000)); // Wh → mWh
        assert_eq!(scale_decimal("41.30", 2), Some(4_130)); // Cel → centi-°C
    }

    #[test]
    fn scales_signs_zero_shift_and_rounding() {
        assert_eq!(scale_decimal("-3.5", 3), Some(-3_500));
        assert_eq!(scale_decimal("1751625000", 0), Some(1_751_625_000));
        // Surplus precision rounds half-away-from-zero.
        assert_eq!(scale_decimal("1.2345", 3), Some(1_235));
        assert_eq!(scale_decimal("-1.2345", 3), Some(-1_235));
        assert_eq!(scale_decimal("1.2344", 3), Some(1_234));
    }

    #[test]
    fn rejects_non_decimal_forms() {
        assert_eq!(scale_decimal("1e3", 3), None);
        assert_eq!(scale_decimal("", 3), None);
        assert_eq!(scale_decimal(".", 3), None);
        assert_eq!(scale_decimal("abc", 3), None);
        // i64 overflow is refused, not wrapped.
        assert_eq!(scale_decimal("99999999999999999999", 3), None);
    }

    #[test]
    fn parses_the_contract_example_pack() {
        let payload = br#"[
          {"bn": "urn:dev:ser:2130012345:", "bt": 1751625000, "bu": "W"},
          {"n": "ac/total_power",                        "v": 2450},
          {"n": "ac/l1/voltage",            "u": "V",    "v": 236.40},
          {"n": "dc/s1/power",              "t": -10,    "v": 1310},
          {"n": "energy/total",             "u": "Wh",   "v": 41784321},
          {"n": "device/status",                         "vs": "ok"},
          {"n": "device/grid_relay",                     "vs": "closed"}
        ]"#;
        let pack = parse_pack(payload).unwrap();
        assert_eq!(pack.base_time, 1_751_625_000);
        assert_eq!(pack.records.len(), 6);

        let power = &pack.records[0];
        assert_eq!(power.name, "ac/total_power");
        // Watt records omit `u`; the pack default (`bu`) applies.
        assert_eq!(power.unit.as_deref(), Some("W"));
        assert_eq!(power.value, Value::Number("2450".into()));

        let voltage = &pack.records[1];
        assert_eq!(voltage.unit.as_deref(), Some("V"));
        // Trailing zeros survive byte-for-byte.
        assert_eq!(voltage.value, Value::Number("236.40".into()));

        let dc = &pack.records[2];
        assert_eq!(dc.time_offset, -10);

        let relay = &pack.records[5];
        assert_eq!(relay.value, Value::Text("closed".into()));
    }

    #[test]
    fn pack_without_header_is_an_error() {
        let payload = br#"[{"n": "ac/total_power", "v": 1}]"#;
        assert!(matches!(parse_pack(payload), Err(PackError::MissingHeader)));
    }
}
