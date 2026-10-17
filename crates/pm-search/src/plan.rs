//! Trigram plans: which trigrams a text must contain to possibly match a
//! substring or regular-expression query (Russ Cox's "Regular Expression
//! Matching with a Trigram Index", simplified).
//!
//! A plan is a boolean formula over trigrams of the lower-cased text.
//! [`Plan::All`] means "no restriction" (the query must scan). Plans are
//! conservative: a text that matches the query always satisfies its plan
//! (property-tested); the index then verifies candidates exactly.

use std::collections::BTreeSet;

use regex_syntax::hir::{Class, Hir, HirKind};

use crate::tokenize::{is_indexed_trigram, lower_chars, query_trigrams};

/// Exact-string sets larger than this are given up (become unknown).
const MAX_SET: usize = 64;
/// Characters a class may have to be expanded into an exact set.
const MAX_CLASS: usize = 8;

/// A boolean formula over trigrams.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Plan {
    /// Matches everything.
    All,
    Trigram(String),
    And(Vec<Plan>),
    Or(Vec<Plan>),
}

impl Plan {
    fn and(parts: Vec<Plan>) -> Plan {
        let mut out: Vec<Plan> = Vec::new();
        for p in parts {
            match p {
                Plan::All => {}
                Plan::And(inner) => out.extend(inner),
                other => out.push(other),
            }
        }
        out.sort();
        out.dedup();
        match out.len() {
            0 => Plan::All,
            1 => out.pop().unwrap_or(Plan::All),
            _ => Plan::And(out),
        }
    }

    fn or(parts: Vec<Plan>) -> Plan {
        let mut out: Vec<Plan> = Vec::new();
        for p in parts {
            match p {
                Plan::All => return Plan::All,
                Plan::Or(inner) => out.extend(inner),
                other => out.push(other),
            }
        }
        out.sort();
        out.dedup();
        match out.len() {
            0 => Plan::All,
            1 => out.pop().unwrap_or(Plan::All),
            _ => Plan::Or(out),
        }
    }

    /// Whether a text with these (lower-cased) trigrams satisfies the plan.
    pub fn satisfied_by(&self, has: &dyn Fn(&str) -> bool) -> bool {
        match self {
            Plan::All => true,
            Plan::Trigram(t) => has(t),
            Plan::And(v) => v.iter().all(|p| p.satisfied_by(has)),
            Plan::Or(v) => v.iter().any(|p| p.satisfied_by(has)),
        }
    }

    /// The plan of a literal substring: all its trigrams.
    pub fn for_literal(term: &str) -> Plan {
        let lower = lower_chars(term);
        Plan::and(
            query_trigrams(&lower)
                .into_iter()
                .filter(|(_, t)| is_indexed_trigram(t))
                .map(|(_, t)| Plan::Trigram(t))
                .collect(),
        )
    }

    /// The plan of a regular expression (parsed case-insensitively, as
    /// keyword regexes are); `All` if it cannot be parsed.
    pub fn for_regex(pattern: &str) -> Plan {
        let hir = regex_syntax::ParserBuilder::new()
            .case_insensitive(true)
            .build()
            .parse(pattern);
        match hir {
            Ok(h) => {
                let info = analyze(&h, 0);
                Plan::and(vec![info.plan, exact_plan(info.exact.as_ref())])
            }
            Err(_) => Plan::All,
        }
    }
}

/// What is known about the strings a sub-expression matches.
struct Info {
    /// Every string it can match (lower-cased), if few.
    exact: Option<BTreeSet<String>>,
    /// A plan every match satisfies.
    plan: Plan,
}

fn unknown(plan: Plan) -> Info {
    Info { exact: None, plan }
}

fn exact(set: BTreeSet<String>) -> Info {
    Info {
        exact: Some(set),
        plan: Plan::All,
    }
}

/// The plan of an exact set: one of its strings is present.
fn exact_plan(set: Option<&BTreeSet<String>>) -> Plan {
    match set {
        None => Plan::All,
        Some(s) => Plan::or(s.iter().map(|x| Plan::for_literal(x)).collect()),
    }
}

fn analyze(h: &Hir, depth: u32) -> Info {
    if depth > 64 {
        return unknown(Plan::All);
    }
    match h.kind() {
        HirKind::Empty | HirKind::Look(_) => exact(BTreeSet::from([String::new()])),
        HirKind::Literal(lit) => match std::str::from_utf8(&lit.0) {
            Ok(s) => exact(BTreeSet::from([lower_chars(s)])),
            Err(_) => unknown(Plan::All),
        },
        HirKind::Class(c) => class_info(c),
        HirKind::Capture(cap) => analyze(&cap.sub, depth + 1),
        HirKind::Repetition(rep) => {
            if rep.min == 0 {
                return unknown(Plan::All);
            }
            let sub = analyze(&rep.sub, depth + 1);
            if rep.max == Some(1) {
                return sub;
            }
            // At least one copy: its plan holds.
            unknown(Plan::and(vec![sub.plan, exact_plan(sub.exact.as_ref())]))
        }
        HirKind::Concat(parts) => {
            let mut plans = Vec::new();
            let mut cur: Option<BTreeSet<String>> = Some(BTreeSet::from([String::new()]));
            for p in parts {
                let info = analyze(p, depth + 1);
                plans.push(info.plan);
                cur = match (cur, info.exact) {
                    (Some(a), Some(b)) if a.len().saturating_mul(b.len()) <= MAX_SET => {
                        Some(cross(&a, &b))
                    }
                    (Some(a), b) => {
                        // Give up the run so far: its strings must appear.
                        plans.push(exact_plan(Some(&a)));
                        b
                    }
                    (None, b) => b,
                };
            }
            Info {
                exact: cur,
                plan: Plan::and(plans),
            }
        }
        HirKind::Alternation(alts) => {
            let infos: Vec<Info> = alts.iter().map(|a| analyze(a, depth + 1)).collect();
            let mut union = BTreeSet::new();
            let mut all_exact = true;
            for i in &infos {
                match &i.exact {
                    Some(s) => union.extend(s.iter().cloned()),
                    None => all_exact = false,
                }
            }
            if all_exact && union.len() <= MAX_SET && infos.iter().all(|i| i.plan == Plan::All) {
                return exact(union);
            }
            unknown(Plan::or(
                infos
                    .into_iter()
                    .map(|i| Plan::and(vec![i.plan, exact_plan(i.exact.as_ref())]))
                    .collect(),
            ))
        }
    }
}

fn cross(a: &BTreeSet<String>, b: &BTreeSet<String>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for x in a {
        for y in b {
            out.insert(format!("{x}{y}"));
        }
    }
    out
}

fn class_info(c: &Class) -> Info {
    let mut set = BTreeSet::new();
    match c {
        Class::Unicode(u) => {
            let mut n = 0usize;
            for r in u.iter() {
                n += (r.end() as usize).saturating_sub(r.start() as usize) + 1;
                if n > MAX_CLASS * 4 {
                    return unknown(Plan::All);
                }
                for cp in u32::from(r.start())..=u32::from(r.end()) {
                    if let Some(ch) = char::from_u32(cp) {
                        set.insert(ch.to_lowercase().collect::<String>());
                    }
                }
            }
        }
        Class::Bytes(b) => {
            let mut n = 0usize;
            for r in b.iter() {
                n += usize::from(r.end()).saturating_sub(usize::from(r.start())) + 1;
                if n > MAX_CLASS * 4 {
                    return unknown(Plan::All);
                }
                for byte in r.start()..=r.end() {
                    if !byte.is_ascii() {
                        return unknown(Plan::All);
                    }
                    set.insert(char::from(byte).to_ascii_lowercase().to_string());
                }
            }
        }
    }
    if set.len() > MAX_CLASS {
        return unknown(Plan::All);
    }
    exact(set)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::tokenize::{lower_chars, trigrams};

    fn t(s: &str) -> Plan {
        Plan::Trigram(s.to_owned())
    }

    #[test]
    fn literal_plans() {
        assert_eq!(Plan::for_literal("ab"), Plan::All);
        assert_eq!(
            Plan::for_literal("a   b"),
            Plan::And(vec![t("  b"), t("a  ")])
        );
        assert_eq!(Plan::for_literal("ABC"), t("abc"));
        assert_eq!(
            Plan::for_literal("abcd"),
            Plan::And(vec![t("abc"), t("bcd")])
        );
    }

    #[test]
    fn regex_plans() {
        assert_eq!(Plan::for_regex("hello"), Plan::for_literal("hello"));
        assert_eq!(Plan::for_regex(r"\d+"), Plan::All);
        assert_eq!(Plan::for_regex("("), Plan::All);
        assert_eq!(
            Plan::for_regex("abc|xyz"),
            Plan::Or(vec![t("abc"), t("xyz")])
        );
        // Case-insensitive classes collapse.
        assert_eq!(Plan::for_regex("[Aa]bc"), t("abc"));
        // Required repetition keeps its plan; optional loses it.
        assert_eq!(Plan::for_regex("(abc)+x"), t("abc"));
        assert_eq!(Plan::for_regex("(abc)*x"), Plan::All);
        // Sequences around unknown parts.
        assert_eq!(
            Plan::for_regex(r"word\d+code"),
            Plan::And(vec![t("cod"), t("ode"), t("ord"), t("wor")])
        );
        // `(?i)s` also matches U+017F (long s), which stays distinct.
        assert_eq!(
            Plan::for_regex("sun"),
            Plan::Or(vec![t("sun"), t("\u{17F}un")])
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(300))]

        /// A text that matches never fails its plan.
        #[test]
        fn plans_never_exclude_matches(
            pattern in "(([a-c]|[xX]|ab|bc|\\.|\\d|\\s){1,4}([*+?]|\\{1,2\\})?(\\|)?){1,5}",
            text in "[a-cxX0-9 .]{0,40}",
        ) {
            let Ok(re) = regex::RegexBuilder::new(&pattern).case_insensitive(true).build() else {
                return Ok(());
            };
            if re.is_match(&text) {
                let tri: BTreeSet<String> = trigrams(&lower_chars(&text)).into_iter().collect();
                let plan = Plan::for_regex(&pattern);
                prop_assert!(plan.satisfied_by(&|g| tri.contains(g)), "{pattern} {text:?} {plan:?}");
            }
        }
    }
}
