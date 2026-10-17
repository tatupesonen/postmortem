//! Tool arguments: typed access, times, stable references and cursors.

use base64::Engine as _;
use serde_json::{Map, Value};

use crate::error::{ToolError, ToolResult};

/// Default page size.
pub const DEFAULT_LIMIT: usize = 50;
/// Largest page size.
pub const MAX_LIMIT: usize = 500;

/// The `arguments` object of a call.
#[derive(Debug, Clone, Default)]
pub struct Args(pub Map<String, Value>);

impl Args {
    pub fn new(map: Map<String, Value>) -> Self {
        Self(map)
    }

    fn present(&self, key: &str) -> Option<&Value> {
        self.0.get(key).filter(|v| !v.is_null())
    }

    /// A trimmed, non-empty string.
    pub fn str(&self, key: &str) -> ToolResult<Option<String>> {
        match self.present(key) {
            None => Ok(None),
            Some(Value::String(s)) => {
                let t = s.trim();
                Ok((!t.is_empty()).then(|| t.to_owned()))
            }
            Some(Value::Number(n)) => Ok(Some(n.to_string())),
            Some(_) => Err(ToolError::arg(format!("{key} must be a string"))),
        }
    }

    pub fn req_str(&self, key: &str) -> ToolResult<String> {
        self.str(key)?
            .ok_or_else(|| ToolError::arg(format!("{key} is required")))
    }

    /// An integer (numbers and numeric strings are accepted).
    pub fn i64(&self, key: &str) -> ToolResult<Option<i64>> {
        match self.present(key) {
            None => Ok(None),
            Some(Value::Number(n)) => n
                .as_i64()
                .or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))
                .map(Some)
                .ok_or_else(|| ToolError::arg(format!("{key} must be an integer"))),
            Some(Value::String(s)) => {
                let t = s.trim();
                if t.is_empty() {
                    return Ok(None);
                }
                t.parse()
                    .map(Some)
                    .map_err(|_| ToolError::arg(format!("{key} must be an integer")))
            }
            _ => Err(ToolError::arg(format!("{key} must be an integer"))),
        }
    }

    pub fn req_i64(&self, key: &str) -> ToolResult<i64> {
        self.i64(key)?
            .ok_or_else(|| ToolError::arg(format!("{key} is required")))
    }

    /// A non-negative integer.
    pub fn u64(&self, key: &str) -> ToolResult<Option<u64>> {
        match self.i64(key)? {
            None => Ok(None),
            Some(v) => u64::try_from(v)
                .map(Some)
                .map_err(|_| ToolError::arg(format!("{key} must not be negative"))),
        }
    }

    pub fn bool(&self, key: &str) -> ToolResult<Option<bool>> {
        match self.present(key) {
            None => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
                "true" | "yes" | "1" => Ok(Some(true)),
                "false" | "no" | "0" => Ok(Some(false)),
                "" => Ok(None),
                _ => Err(ToolError::arg(format!("{key} must be true or false"))),
            },
            Some(_) => Err(ToolError::arg(format!("{key} must be true or false"))),
        }
    }

    pub fn flag(&self, key: &str) -> ToolResult<bool> {
        Ok(self.bool(key)?.unwrap_or(false))
    }

    /// A list of strings (a JSON array, or a comma-separated string).
    pub fn str_list(&self, key: &str) -> ToolResult<Vec<String>> {
        match self.present(key) {
            None => Ok(Vec::new()),
            Some(Value::Array(a)) => a
                .iter()
                .map(|v| match v {
                    Value::String(s) => Ok(s.trim().to_owned()),
                    Value::Number(n) => Ok(n.to_string()),
                    _ => Err(ToolError::arg(format!("{key} must be a list of strings"))),
                })
                .filter(|r| r.as_ref().map_or(true, |s| !s.is_empty()))
                .collect(),
            Some(Value::String(s)) => Ok(s
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()),
            Some(_) => Err(ToolError::arg(format!("{key} must be a list of strings"))),
        }
    }

    /// `limit` with a default, clamped to `1..=max`.
    pub fn limit(&self, default: usize, max: usize) -> ToolResult<usize> {
        Ok(match self.i64("limit")? {
            Some(n) if n > 0 => (n as usize).min(max),
            _ => default,
        })
    }

    /// A time argument (ISO 8601, a date, or Unix seconds). Date-only
    /// values are expanded to the start of the day, or its last second for
    /// end boundaries (Autopsy's `parseTimeArg`).
    pub fn time(&self, key: &str, end_boundary: bool) -> ToolResult<Option<i64>> {
        match self.present(key) {
            None => Ok(None),
            Some(Value::Number(n)) => n
                .as_i64()
                .map(Some)
                .ok_or_else(|| ToolError::arg(format!("{key} must be a time"))),
            Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
            Some(Value::String(s)) => parse_time(s.trim(), end_boundary)
                .map(Some)
                .ok_or_else(|| {
                    ToolError::arg(format!(
                        "{key}: cannot parse {s:?} as ISO 8601 (e.g. 2021-03-04T09:00:00Z), a date or Unix seconds"
                    ))
                }),
            Some(_) => Err(ToolError::arg(format!("{key} must be a time"))),
        }
    }

    /// The opaque `cursor` argument.
    pub fn cursor(&self) -> ToolResult<Option<Value>> {
        match self.str("cursor")? {
            None => Ok(None),
            Some(c) => decode_cursor(&c)
                .map(Some)
                .ok_or_else(|| ToolError::arg("cursor is not a cursor returned by this server")),
        }
    }
}

/// Parses ISO 8601 / RFC 3339, `YYYY-MM-DD` or Unix seconds.
pub fn parse_time(s: &str, end_boundary: bool) -> Option<i64> {
    if let Ok(n) = s.parse::<i64>() {
        return Some(n);
    }
    if let Ok(ts) = s.parse::<jiff::Timestamp>() {
        return Some(ts.as_second());
    }
    if s.len() == 10
        && let Ok(d) = s.parse::<jiff::civil::Date>()
    {
        let t = if end_boundary {
            d.at(23, 59, 59, 0)
        } else {
            d.at(0, 0, 0, 0)
        };
        return t
            .to_zoned(jiff::tz::TimeZone::UTC)
            .ok()
            .map(|z| z.timestamp().as_second());
    }
    // A local date-time without an offset is taken as UTC.
    if let Ok(dt) = s.parse::<jiff::civil::DateTime>() {
        return dt
            .to_zoned(jiff::tz::TimeZone::UTC)
            .ok()
            .map(|z| z.timestamp().as_second());
    }
    None
}

/// ISO 8601 UTC text of Unix seconds (`None` for 0 and out-of-range
/// values, as Autopsy's `epochToIso`).
pub fn iso(epoch: i64) -> Option<String> {
    if epoch == 0 {
        return None;
    }
    jiff::Timestamp::from_second(epoch)
        .ok()
        .map(|t| t.to_string())
}

/// `iso` as a JSON value (`null` when absent).
pub fn iso_json(epoch: Option<i64>) -> Value {
    epoch.and_then(iso).map_or(Value::Null, Value::String)
}

/// Encodes a cursor value as an opaque string.
pub fn encode_cursor(v: &Value) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string())
}

fn decode_cursor(s: &str) -> Option<Value> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s.trim())
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// A stable reference to a case object (`file:123`, `artifact:45`, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ref {
    File(i64),
    /// Data artifacts and analysis results (`artifact:` / `result:`).
    Artifact(i64),
    Account(i64),
    OsAccount(i64),
    Event(i64),
    DataSource(i64),
    Host(i64),
    Report(i64),
}

impl Ref {
    /// Parses `kind:id`; a bare number is a file id (Autopsy's object ids).
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if let Ok(id) = s.parse() {
            return Some(Self::File(id));
        }
        let (kind, id) = s.split_once(':')?;
        let id: i64 = id.trim().parse().ok()?;
        Some(match kind.trim().to_ascii_lowercase().as_str() {
            "file" => Self::File(id),
            "artifact" | "result" => Self::Artifact(id),
            "account" => Self::Account(id),
            "os_account" => Self::OsAccount(id),
            "event" => Self::Event(id),
            "ds" | "data_source" => Self::DataSource(id),
            "host" => Self::Host(id),
            "report" => Self::Report(id),
            _ => return None,
        })
    }

    /// The `ref` argument.
    pub fn arg(args: &Args, key: &str) -> ToolResult<Option<Self>> {
        match args.str(key)? {
            None => Ok(None),
            Some(s) => Self::parse(&s).map(Some).ok_or_else(|| {
                ToolError::arg(format!(
                    "{key}: {s:?} is not a reference such as file:123 or artifact:45"
                ))
            }),
        }
    }

    pub fn req(args: &Args, key: &str) -> ToolResult<Self> {
        Self::arg(args, key)?.ok_or_else(|| ToolError::arg(format!("{key} is required")))
    }
}

impl std::fmt::Display for Ref {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::File(i) => write!(f, "file:{i}"),
            Self::Artifact(i) => write!(f, "artifact:{i}"),
            Self::Account(i) => write!(f, "account:{i}"),
            Self::OsAccount(i) => write!(f, "os_account:{i}"),
            Self::Event(i) => write!(f, "event:{i}"),
            Self::DataSource(i) => write!(f, "ds:{i}"),
            Self::Host(i) => write!(f, "host:{i}"),
            Self::Report(i) => write!(f, "report:{i}"),
        }
    }
}

/// Lower-case hex.
pub fn hex(b: &[u8]) -> String {
    use std::fmt::Write as _;
    b.iter()
        .fold(String::with_capacity(b.len() * 2), |mut s, x| {
            let _ = write!(s, "{x:02x}");
            s
        })
}

/// Parses a hex hash; `None` when it is not hex of a hash length.
pub fn unhex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) || !matches!(s.len(), 32 | 40 | 64) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| s.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[allow(clippy::needless_pass_by_value, reason = "test helper")]
    fn args(v: Value) -> Args {
        Args::new(v.as_object().cloned().unwrap_or_default())
    }

    #[test]
    fn times() {
        assert_eq!(parse_time("1970-01-02", false), Some(86_400));
        assert_eq!(parse_time("1970-01-02", true), Some(86_400 + 86_399));
        assert_eq!(parse_time("1970-01-01T00:01:00Z", false), Some(60));
        assert_eq!(parse_time("1970-01-01T02:00:00+02:00", false), Some(0));
        assert_eq!(parse_time("1970-01-01T00:00:10", false), Some(10));
        assert_eq!(parse_time("1234", false), Some(1234));
        assert_eq!(parse_time("yesterday", false), None);
        assert_eq!(iso(60).as_deref(), Some("1970-01-01T00:01:00Z"));
        assert_eq!(iso(0), None);
    }

    #[test]
    fn typed_args() {
        let a = args(json!({"n": "12", "f": 3.0, "b": "yes", "l": "a, b,", "s": "  ", "bad": [1]}));
        assert_eq!(a.i64("n").unwrap(), Some(12));
        assert_eq!(a.i64("f").unwrap(), Some(3));
        assert_eq!(a.bool("b").unwrap(), Some(true));
        assert_eq!(a.str_list("l").unwrap(), vec!["a", "b"]);
        assert_eq!(a.str("s").unwrap(), None);
        assert!(a.i64("bad").is_err());
        assert!(a.req_str("missing").is_err());
        assert_eq!(args(json!({"limit": 10_000})).limit(50, 500).unwrap(), 500);
        assert_eq!(args(json!({"limit": 0})).limit(50, 500).unwrap(), 50);
    }

    #[test]
    fn refs_and_cursors() {
        for r in [
            "file:1",
            "artifact:2",
            "account:3",
            "os_account:4",
            "event:5",
            "ds:6",
            "host:7",
            "report:8",
        ] {
            assert_eq!(Ref::parse(r).unwrap().to_string(), r);
        }
        assert_eq!(Ref::parse("result:9"), Some(Ref::Artifact(9)));
        assert_eq!(Ref::parse("42"), Some(Ref::File(42)));
        assert_eq!(Ref::parse("nope:1"), None);
        let c = json!([1, "x"]);
        let a = args(json!({"cursor": encode_cursor(&c)}));
        assert_eq!(a.cursor().unwrap(), Some(c));
        assert!(args(json!({"cursor": "!!"})).cursor().is_err());
        assert_eq!(unhex("00ff00ff00ff00ff00ff00ff00ff00ff").unwrap().len(), 16);
        assert_eq!(hex(&[0, 255]), "00ff");
        assert!(unhex("abc").is_none());
    }
}
