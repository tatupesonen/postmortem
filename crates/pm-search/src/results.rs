//! `KAAMOS_KEYWORD_HIT` results exactly as Autopsy writes them
//! (`LuceneQuery` / `RegexQuery.createKeywordHitArtifact`), shared by the
//! ingest module and the ad-hoc "Save search results" action.

use pm_casedb::catalog::{artifact_type_id, attribute_type_id};
use pm_casedb::{KeywordHitKey, NewArtifact, Priority, Significance, Value};

use crate::keywords::SearchType;

/// One hit to record.
#[derive(Debug, Clone, Copy)]
pub struct KeywordHitSpec<'a> {
    pub ds_id: i64,
    /// The file the hit is in (for an artifact hit: the artifact's file).
    pub file_id: Option<i64>,
    /// Set when the hit is in an artifact's text.
    pub associated_artifact: Option<i64>,
    /// Keyword list (empty for an ad-hoc single term).
    pub list_name: &'a str,
    /// The keyword as configured.
    pub term: &'a str,
    pub search_type: SearchType,
    /// Selector attribute type (`KAAMOS_EMAIL`, …), if any.
    pub selector: Option<&'a str>,
    /// The matched text.
    pub hit: &'a str,
    /// Autopsy's preview snippet.
    pub snippet: &'a str,
    /// Attribute source (`"Keyword Search"`).
    pub module: &'a str,
    pub created_at: i64,
}

fn attr(name: &str) -> i64 {
    attribute_type_id(name).unwrap_or(0)
}

/// The analysis result (score Likely Notable, configuration = list name)
/// and its identity for de-duplication.
pub fn keyword_hit_artifact(s: &KeywordHitSpec<'_>) -> (NewArtifact, KeywordHitKey) {
    let m = || s.module.to_owned();
    let text = |name: &str, v: &str| (attr(name), Value::Text(v.to_owned()), m());
    let keyword = s.hit.to_lowercase();
    let has_list = !s.list_name.trim().is_empty();
    let mut attributes: Vec<(i64, Value, String)> = Vec::new();
    let (search_type, regexp) = if s.search_type == SearchType::Exact {
        // LuceneQuery.createKeywordHitArtifact.
        attributes.push(text("KAAMOS_KEYWORD_PREVIEW", s.snippet));
        attributes.push(text("KAAMOS_KEYWORD", &keyword));
        if has_list {
            attributes.push(text("KAAMOS_SET_NAME", s.list_name));
        }
        if let Some(sel) = s.selector
            && attr(sel) != 0
        {
            attributes.push(text(sel, s.hit));
        }
        attributes.push((attr("KAAMOS_KEYWORD_SEARCH_TYPE"), Value::Int(0), m()));
        (0, None)
    } else {
        // RegexQuery.createKeywordHitArtifact.
        attributes.push(text("KAAMOS_KEYWORD", &keyword));
        attributes.push(text("KAAMOS_KEYWORD_REGEXP", s.term));
        if has_list {
            attributes.push(text("KAAMOS_SET_NAME", s.list_name));
        }
        attributes.push(text("KAAMOS_KEYWORD_PREVIEW", s.snippet));
        let st = if s.search_type == SearchType::Substring {
            1
        } else {
            2
        };
        (st, Some(s.term.to_owned()))
    };
    if let Some(a) = s.associated_artifact {
        attributes.push((attr("KAAMOS_ASSOCIATED_ARTIFACT"), Value::Int(a), m()));
    }
    if s.search_type != SearchType::Exact {
        attributes.push((
            attr("KAAMOS_KEYWORD_SEARCH_TYPE"),
            Value::Int(search_type),
            m(),
        ));
    }
    let key = KeywordHitKey {
        keyword,
        set_name: has_list.then(|| s.list_name.to_owned()),
        search_type: Some(search_type),
        regexp,
        associated_artifact: s.associated_artifact,
    };
    let artifact = NewArtifact {
        type_id: artifact_type_id("KAAMOS_KEYWORD_HIT").unwrap_or(9),
        ds_id: s.ds_id,
        file_id: s.file_id,
        source_artifact_id: None,
        score: Some((Significance::LikelyNotable, Priority::Normal)),
        conclusion: None,
        configuration: Some(s.list_name.to_owned()),
        justification: None,
        created_at: s.created_at,
        attributes,
    };
    (artifact, key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(st: SearchType) -> KeywordHitSpec<'static> {
        KeywordHitSpec {
            ds_id: 1,
            file_id: Some(2),
            associated_artifact: None,
            list_name: "L",
            term: "ell",
            search_type: st,
            selector: None,
            hit: "Hello",
            snippet: "x «hello« y",
            module: "Keyword Search",
            created_at: 0,
        }
    }

    fn names(a: &NewArtifact) -> Vec<i64> {
        a.attributes.iter().map(|x| x.0).collect()
    }

    #[test]
    fn exact_and_regex_layouts() {
        let (a, k) = keyword_hit_artifact(&spec(SearchType::Exact));
        assert_eq!(
            names(&a),
            [
                attr("KAAMOS_KEYWORD_PREVIEW"),
                attr("KAAMOS_KEYWORD"),
                attr("KAAMOS_SET_NAME"),
                attr("KAAMOS_KEYWORD_SEARCH_TYPE")
            ]
        );
        assert_eq!(k.search_type, Some(0));
        assert_eq!(k.keyword, "hello");
        assert_eq!(a.configuration.as_deref(), Some("L"));
        assert_eq!(
            a.score,
            Some((Significance::LikelyNotable, Priority::Normal))
        );

        let mut s = spec(SearchType::Substring);
        s.associated_artifact = Some(7);
        s.list_name = "";
        let (a, k) = keyword_hit_artifact(&s);
        assert_eq!(
            names(&a),
            [
                attr("KAAMOS_KEYWORD"),
                attr("KAAMOS_KEYWORD_REGEXP"),
                attr("KAAMOS_KEYWORD_PREVIEW"),
                attr("KAAMOS_ASSOCIATED_ARTIFACT"),
                attr("KAAMOS_KEYWORD_SEARCH_TYPE")
            ]
        );
        assert_eq!(
            (k.search_type, k.set_name, k.regexp.as_deref()),
            (Some(1), None, Some("ell"))
        );
        let (_, k) = keyword_hit_artifact(&spec(SearchType::Regex));
        assert_eq!(k.search_type, Some(2));

        let mut s = spec(SearchType::Exact);
        s.selector = Some("KAAMOS_EMAIL");
        let (a, _) = keyword_hit_artifact(&s);
        assert!(
            a.attributes
                .iter()
                .any(|x| x.0 == attr("KAAMOS_EMAIL") && x.1 == Value::Text("Hello".into()))
        );
    }
}
