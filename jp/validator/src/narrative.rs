// SPDX-License-Identifier: Apache-2.0
use helios_fhir_validator::ConstraintOutcome;
use serde_json::Value;

/// R4 Narrative txt-1/txt-2, including the XHTML namespace and safe element set.
pub fn evaluate(root: &Value, path: &str, id: &str) -> ConstraintOutcome {
    let mut value = root;
    for segment in path.split('.').skip(1) {
        value = if let Ok(i) = segment.parse::<usize>() {
            &value[i]
        } else {
            &value[segment]
        };
    }
    let Some(html) = value.as_str() else {
        return ConstraintOutcome::NotEvaluable("narrative focus is not a string".into());
    };
    let Ok(doc) = roxmltree::Document::parse(html) else {
        return ConstraintOutcome::Failed;
    };
    let root = doc.root_element();
    if root.tag_name().name() != "div"
        || root.tag_name().namespace() != Some("http://www.w3.org/1999/xhtml")
    {
        return ConstraintOutcome::Failed;
    }
    if doc
        .descendants()
        .filter(|n| n.is_element())
        .any(|n| n.attribute("style").is_some_and(|s| s.contains('\\')))
    {
        return ConstraintOutcome::NotEvaluable(
            "CSS escape interpretation is not implemented".into(),
        );
    }
    let passed = match id {
        "txt-1" => {
            let tags = "a abbr acronym b big blockquote br caption cite code col colgroup dd dfn div dl dt em h1 h2 h3 h4 h5 h6 hr i img li ol p pre q samp small span strong sub sup table tbody td tfoot th thead tr tt ul var";
            let attrs = "abbr accesskey align alt axis bgcolor border cellhalign cellpadding cellspacing cellvalign char charoff charset cite class colspan compact coords dir frame headers height href hreflang hspace id lang longdesc name nowrap rel rev rowspan rules scope shape span src start style summary tabindex title type valign value vspace width";
            !doc.descendants().any(|n| n.is_pi())
                && doc.descendants().filter(|n| n.is_element()).all(|n| {
                    n.tag_name().namespace() == Some("http://www.w3.org/1999/xhtml")
                        && tags.split_whitespace().any(|t| t == n.tag_name().name())
                        && n.attributes().all(|a| {
                            let attribute_ok =
                                attrs.split_whitespace().any(|name| name == a.name())
                                    && a.namespace().is_none_or(|ns| {
                                        ns == "http://www.w3.org/XML/1998/namespace"
                                            && a.name() == "lang"
                                    });
                            let normalized: String = a
                                .value()
                                .chars()
                                .filter(|c| !c.is_ascii_whitespace() && !c.is_ascii_control())
                                .flat_map(char::to_lowercase)
                                .collect();
                            attribute_ok
                                && !normalized.contains("javascript:")
                                && !normalized.contains("vbscript:")
                                && !(a.name() == "style"
                                    && (normalized.contains("expression(")
                                        || normalized.contains("@import")))
                        })
                })
        }
        "txt-2" => doc.descendants().any(|n| {
            n.is_text() && n.text().is_some_and(|s| !s.trim().is_empty())
                || n.is_element() && n.tag_name().name() == "img" && n.attribute("src").is_some()
        }),
        _ => return ConstraintOutcome::NotEvaluable("unknown narrative invariant".into()),
    };
    if passed {
        ConstraintOutcome::Passed
    } else {
        ConstraintOutcome::Failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn validates_xhtml_and_rejects_scripts_empty_text_and_entities() {
        for html in [
            "<div xmlns=\"http://www.w3.org/1999/xhtml\"><p>架空患者</p></div>",
            "<div xmlns=\"http://www.w3.org/1999/xhtml\"><img src=\"#photo\"/></div>",
        ] {
            for id in ["txt-1", "txt-2"] {
                assert_eq!(
                    evaluate(&json!({"text":{"div":html}}), "Patient.text.div", id),
                    ConstraintOutcome::Passed
                );
            }
        }
        for html in [
            "<div>wrong namespace</div>",
            "<div xmlns=\"http://www.w3.org/1999/xhtml\"><script>alert(1)</script></div>",
            "<div xmlns=\"http://www.w3.org/1999/xhtml\" onclick=\"bad()\">text</div>",
            "<!DOCTYPE div [<!ENTITY x 'test'>]><div xmlns=\"http://www.w3.org/1999/xhtml\">&x;</div>",
        ] {
            assert_eq!(
                evaluate(&json!({"text":{"div":html}}), "Patient.text.div", "txt-1"),
                ConstraintOutcome::Failed
            );
        }
        assert_eq!(
            evaluate(
                &json!({"text":{"div":"<div xmlns=\"http://www.w3.org/1999/xhtml\"> </div>"}}),
                "Patient.text.div",
                "txt-2"
            ),
            ConstraintOutcome::Failed
        );
    }
}
