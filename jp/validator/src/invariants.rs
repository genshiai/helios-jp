// SPDX-License-Identifier: Apache-2.0
//! Evaluate only provably trivial branches of exact R4 invariants. All other
//! expressions go through FHIRPath or remain incomplete.
use helios_fhir_validator::{ConstraintOutcome, DeferredConstraint};
use serde_json::Value;
use sha2::{Digest, Sha256};

const DOM3: &str = "contained.where((('#'+id in (%resource.descendants().reference | %resource.descendants().as(canonical) | %resource.descendants().as(uri) | %resource.descendants().as(url))) or descendants().where(reference = '#').exists() or descendants().where(as(canonical) = '#').exists() or descendants().where(as(canonical) = '#').exists()).not()).trace('unmatched', id).empty()";
const REF1: &str = "reference.startsWith('#').not() or (reference.substring(1).trace('url') in %rootResource.contained.id.trace('ids'))";
const OBS7: &str = "value.empty() or component.code.where(coding.intersect(%resource.code.coding).exists()).empty()";
const CON4: &str = "abatement.empty() or clinicalStatus.coding.where(system='http://terminology.hl7.org/CodeSystem/condition-clinical' and (code='resolved' or code='remission' or code='inactive')).exists()";

/// Execute the exact pinned CLINS expression locally; never fetch terminology.
pub fn local(
    resource: &Value,
    c: &DeferredConstraint<'_>,
    tx: &crate::terminology::Terminology,
) -> Option<ConstraintOutcome> {
    if c.id != "check-LTS-PTS-MemberOf-infectionLabo"
        || hex::encode(Sha256::digest(c.expression.as_bytes()))
            != "26853255370ac32427744c6c77e02fa36744f926dfd564b0a72cb45d6124944e"
    {
        return None;
    }
    let node = focus(resource, c.path)?;
    let mut unknown = false;
    for observation in node["entry"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|e| &e["resource"])
        .filter(|r| r["resourceType"] == "Observation")
    {
        let tags: Vec<_> = observation["meta"]["tag"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| {
                t["system"] == "http://jpfhir.jp/fhir/clins/CodeSystem/JP_ehrshrs_indication"
            })
            .collect();
        if tags.is_empty() {
            continue;
        }
        if !tags
            .iter()
            .any(|t| t["code"] == "LTS" || t["code"] == "PTS")
        {
            return Some(ConstraintOutcome::Failed);
        }
        let checks:Vec<_>=observation["code"]["coding"].as_array().into_iter().flatten().flat_map(|coding|["JLAC10","JLAC11"].map(|kind|tx.member(&format!("http://jpfhir.jp/fhir/clins/ValueSet/{kind}/JP_CLINS_ObsLabResult_InfectionLabo_VS"),coding))).collect();
        if checks.contains(&Some(true)) {
            continue;
        }
        if checks.contains(&None) {
            unknown = true;
        } else {
            return Some(ConstraintOutcome::Failed);
        }
    }
    Some(if unknown {
        ConstraintOutcome::NotEvaluable("感染症用語集のローカル判定を完了できません".into())
    } else {
        ConstraintOutcome::Passed
    })
}

pub fn trivial(resource: &Value, c: &DeferredConstraint<'_>) -> Option<ConstraintOutcome> {
    let node = focus(resource, c.path)?;
    // Exact JP-CLINS 1.13.0 expression. Without sharing indications the left
    // side of its OR is true, so memberOf does not need to execute.
    if c.id == "check-LTS-PTS-MemberOf-infectionLabo"
        && hex::encode(Sha256::digest(c.expression.as_bytes()))
            == "26853255370ac32427744c6c77e02fa36744f926dfd564b0a72cb45d6124944e"
    {
        let observations = node["entry"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|e| &e["resource"])
            .filter(|r| r["resourceType"] == "Observation");
        if observations.clone().all(|r| {
            r["meta"]["tag"].as_array().is_none_or(|tags| {
                !tags.iter().any(|t| {
                    t["system"] == "http://jpfhir.jp/fhir/clins/CodeSystem/JP_ehrshrs_indication"
                })
            })
        }) {
            return Some(ConstraintOutcome::Passed);
        }
    }
    let passed = match (c.id, c.expression) {
        ("dom-3", DOM3) => node["contained"].as_array().is_none_or(|contained| {
            contained.iter().all(|r| {
                r["id"].as_str().is_some_and(|id| {
                    has_reference(node, &format!("#{id}")) || has_reference(r, "#")
                })
            })
        }),
        ("ref-1", REF1) => node["reference"].as_str().is_none_or(|s| {
            !s.starts_with('#')
                || resource["contained"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|r| r["id"].as_str() == s.strip_prefix('#')))
        }),
        ("obs-7", OBS7) => node["component"].as_array().is_none_or(Vec::is_empty),
        ("con-4", CON4) => node
            .as_object()
            .is_some_and(|m| !m.keys().any(|k| k.starts_with("abatement"))),
        _ => false,
    };
    passed.then_some(ConstraintOutcome::Passed)
}

fn has_reference(value: &Value, reference: &str) -> bool {
    match value {
        Value::Object(map) => {
            value["reference"] == reference || map.values().any(|v| has_reference(v, reference))
        }
        Value::Array(values) => values.iter().any(|v| has_reference(v, reference)),
        _ => false,
    }
}

fn focus<'a>(resource: &'a Value, path: &str) -> Option<&'a Value> {
    let mut node = resource;
    for segment in path.split('.').skip(1) {
        node = if let Ok(index) = segment.parse::<usize>() {
            node.get(index)?
        } else {
            node.get(segment)?
        };
    }
    Some(node)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn only_exact_invariants_with_vacuous_branches_are_handled() {
        let c = DeferredConstraint {
            path: "Patient",
            id: "dom-3",
            expression: DOM3,
        };
        assert_eq!(
            trivial(&json!({"resourceType":"Patient"}), &c),
            Some(ConstraintOutcome::Passed)
        );
        assert_eq!(
            trivial(
                &json!({"resourceType":"Patient","contained":[{"resourceType":"Organization","id":"unused"}]}),
                &c
            ),
            None
        );
        let custom = DeferredConstraint {
            path: "Patient",
            id: "dom-3",
            expression: "false",
        };
        assert_eq!(trivial(&json!({"resourceType":"Patient"}), &custom), None);
    }
}
