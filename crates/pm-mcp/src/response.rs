//! Tool output: pages of rows with keyset cursors, the response envelope
//! and the response size cap.

use serde_json::{Map, Value, json};

use crate::args::encode_cursor;

/// Largest serialised tool response.
pub const MAX_RESPONSE_BYTES: usize = 256 * 1024;

/// One result row and the cursor that continues after it.
#[derive(Debug, Clone)]
pub struct Row {
    pub value: Value,
    pub cursor: Value,
}

/// A page of rows.
#[derive(Debug, Clone, Default)]
pub struct Page {
    pub rows: Vec<Row>,
    /// Whether more rows follow the last one.
    pub more: bool,
    /// Extra top-level fields (totals, notes).
    pub extra: Map<String, Value>,
}

impl Page {
    /// Builds a page from up to `limit + 1` fetched items: the extra item
    /// only signals that more rows exist.
    pub fn from_items<T>(
        mut items: Vec<T>,
        limit: usize,
        mut row: impl FnMut(&T) -> crate::error::ToolResult<Row>,
    ) -> crate::error::ToolResult<Self> {
        let more = items.len() > limit;
        items.truncate(limit);
        let rows = items.iter().map(&mut row).collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            rows,
            more,
            extra: Map::new(),
        })
    }

    #[must_use]
    pub fn with(mut self, key: &str, v: Value) -> Self {
        self.extra.insert(key.to_owned(), v);
        self
    }
}

/// What a tool returns.
#[derive(Debug, Clone)]
pub enum Output {
    /// A list (`result` is an array, paged).
    Page(Page),
    /// A single object or value (`result` as is).
    Value(Value),
    /// A PNG picture with its description (`result` is `meta`).
    Image { png: Vec<u8>, meta: Value },
}

impl From<Page> for Output {
    fn from(p: Page) -> Self {
        Self::Page(p)
    }
}

impl From<Value> for Output {
    fn from(v: Value) -> Self {
        Self::Value(v)
    }
}

/// The response envelope (Autopsy's `{caseId, result}` plus paging) and
/// the number of rows it holds; at most [`MAX_RESPONSE_BYTES`] serialised.
pub fn envelope(case_id: &str, out: Output) -> (Value, usize) {
    envelope_capped(case_id, out, MAX_RESPONSE_BYTES)
}

pub(crate) fn envelope_capped(case_id: &str, out: Output, cap: usize) -> (Value, usize) {
    match out {
        Output::Image { meta, .. } => envelope_capped(case_id, Output::Value(meta), cap),
        Output::Value(v) => {
            let mut env = json!({"caseId": case_id, "result": v});
            if size(&env) > cap {
                shrink_strings(&mut env, cap);
                if let Some(o) = env.as_object_mut() {
                    o.insert("truncated".into(), Value::Bool(true));
                    o.insert(
                        "truncated_reason".into(),
                        Value::String(
                            "long text fields were shortened to keep the response under 256 KiB; request a smaller range"
                                .into(),
                        ),
                    );
                }
            }
            (env, 1)
        }
        Output::Page(page) => page_envelope(case_id, &page, cap),
    }
}

fn page_envelope(case_id: &str, page: &Page, cap: usize) -> (Value, usize) {
    let build = |rows: &[Row], next: Option<&Value>, truncated: bool| {
        let mut o = Map::new();
        o.insert("caseId".into(), Value::String(case_id.to_owned()));
        o.insert(
            "result".into(),
            Value::Array(rows.iter().map(|r| r.value.clone()).collect()),
        );
        o.insert(
            "next_cursor".into(),
            next.map_or(Value::Null, |c| Value::String(encode_cursor(c))),
        );
        o.insert("truncated".into(), Value::Bool(truncated));
        if truncated {
            o.insert(
                "truncated_reason".into(),
                Value::String(
                    "response limited to 256 KiB; call again with next_cursor for the remaining rows"
                        .into(),
                ),
            );
        }
        for (k, v) in &page.extra {
            o.insert(k.clone(), v.clone());
        }
        Value::Object(o)
    };
    let next = if page.more {
        page.rows.last().map(|r| &r.cursor)
    } else {
        None
    };
    let full = build(&page.rows, next, false);
    if size(&full) <= cap {
        return (full, page.rows.len());
    }
    // Largest prefix that fits (sizes grow monotonically with the prefix).
    let (mut lo, mut hi) = (0usize, page.rows.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let rows = page.rows.get(..mid).unwrap_or_default();
        if size(&build(rows, rows.last().map(|r| &r.cursor), true)) <= cap {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 {
        // Not even one row fits: shorten its long strings.
        let first = page.rows.first().cloned();
        let rows: Vec<Row> = first.into_iter().collect();
        let mut env = build(&rows, rows.last().map(|r| &r.cursor), true);
        shrink_strings(&mut env, cap);
        return (env, rows.len());
    }
    let rows = page.rows.get(..lo).unwrap_or_default();
    (build(rows, rows.last().map(|r| &r.cursor), true), lo)
}

fn size(v: &Value) -> usize {
    serde_json::to_vec(v).map_or(usize::MAX, |b| b.len())
}

/// Halves the longest strings until the value fits `cap` (or nothing long
/// is left).
fn shrink_strings(v: &mut Value, cap: usize) {
    for _ in 0..64 {
        if size(v) <= cap {
            return;
        }
        let longest = longest_string(v);
        if longest < 64 {
            return;
        }
        cut_strings(v, longest / 2);
    }
}

fn longest_string(v: &Value) -> usize {
    match v {
        Value::String(s) => s.len(),
        Value::Array(a) => a.iter().map(longest_string).max().unwrap_or(0),
        Value::Object(o) => o.values().map(longest_string).max().unwrap_or(0),
        _ => 0,
    }
}

fn cut_strings(v: &mut Value, max: usize) {
    match v {
        Value::String(s) if s.len() > max => {
            let mut end = max;
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            s.truncate(end);
            s.push_str("…[truncated]");
        }
        Value::Array(a) => a.iter_mut().for_each(|x| cut_strings(x, max)),
        Value::Object(o) => o.values_mut().for_each(|x| cut_strings(x, max)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::Args;

    fn page(n: usize, more: bool, len: usize) -> Page {
        Page {
            rows: (0..n)
                .map(|i| Row {
                    value: json!({"i": i, "evidence": {"text": "x".repeat(len)}}),
                    cursor: json!(i),
                })
                .collect(),
            more,
            extra: Map::new(),
        }
    }

    #[test]
    fn small_pages_pass_through() {
        let (env, n) = envelope("c", page(3, true, 10).into());
        assert_eq!(n, 3);
        assert_eq!(env["truncated"], false);
        let a = Args::new(Map::from_iter([(
            "cursor".into(),
            env["next_cursor"].clone(),
        )]));
        assert_eq!(a.cursor().unwrap(), Some(json!(2)));
        let (env, _) = envelope("c", page(3, false, 10).into());
        assert!(env["next_cursor"].is_null());
    }

    #[test]
    fn big_pages_are_cut_at_a_row_with_a_cursor() {
        let (env, n) = envelope_capped("c", page(100, false, 1000).into(), 20_000);
        assert!(n < 100 && n > 0);
        assert!(serde_json::to_vec(&env).unwrap().len() <= 20_000);
        assert_eq!(env["truncated"], true);
        let a = Args::new(Map::from_iter([(
            "cursor".into(),
            env["next_cursor"].clone(),
        )]));
        assert_eq!(a.cursor().unwrap(), Some(json!(n - 1)));
    }

    #[test]
    fn oversized_values_and_rows_are_shortened() {
        let (env, _) = envelope_capped("c", json!({"text": "y".repeat(50_000)}).into(), 10_000);
        assert!(serde_json::to_vec(&env).unwrap().len() <= 10_000);
        assert_eq!(env["truncated"], true);
        let (env, n) = envelope_capped("c", page(2, false, 50_000).into(), 10_000);
        assert_eq!(n, 1);
        assert!(serde_json::to_vec(&env).unwrap().len() <= 10_000);
    }
}
