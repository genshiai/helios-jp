// SPDX-License-Identifier: Apache-2.0
use helios_fhir_validator::{FhirSchema, SchemaResolver};
use serde_json::Value;
use std::collections::HashMap;
use std::{collections::HashSet, sync::Arc};
type Finding = (&'static str, String, String);

pub fn check(root: &Value) -> Vec<Finding> {
    let mut index = HashMap::new();
    let mut findings = Vec::new();
    if let (Some(typ), Some(id)) = (root["resourceType"].as_str(), root["id"].as_str()) {
        index.insert(format!("{typ}/{id}"), root);
    }
    if let Some(entries) = root["entry"].as_array() {
        for (i, entry) in entries.iter().enumerate() {
            let resource = &entry["resource"];
            if let Some(url) = entry["fullUrl"].as_str()
                && index.insert(url.to_owned(), resource).is_some()
            {
                findings.push((
                    "duplicate",
                    format!("Bundle.entry[{i}].fullUrl"),
                    "fullUrlが重複しています".into(),
                ));
            }
            if let (Some(t), Some(id)) =
                (resource["resourceType"].as_str(), resource["id"].as_str())
            {
                index.entry(format!("{t}/{id}")).or_insert(resource);
            }
        }
    }
    walk(root, root, "Resource", &index, &mut findings);
    findings
}

/// The upstream engine carries `targetProfile` but does not enforce it.
/// Walk actual populated elements so an unverified target never silently passes.
pub fn target_profiles(
    root: &Value,
    resolver: &dyn SchemaResolver,
    profiles: &[String],
) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut seen = HashSet::new();
    let mut schemas = profiles.to_vec();
    if let Some(typ) = root["resourceType"].as_str() {
        schemas.push(typ.to_owned());
    }
    for key in schemas {
        if let Some(schema) = resolver.resolve(&key) {
            audit_node(
                root,
                &schema,
                "Resource",
                resolver,
                &mut seen,
                &mut findings,
            );
        }
    }
    findings
}

fn audit_node(
    v: &Value,
    schema: &Arc<FhirSchema>,
    path: &str,
    resolver: &dyn SchemaResolver,
    seen: &mut HashSet<(usize, String)>,
    findings: &mut Vec<Finding>,
) {
    if !seen.insert((Arc::as_ptr(schema) as usize, path.to_owned())) {
        return;
    }
    if let Some(value) = v.as_str() {
        let typ = schema.type_.as_deref().or_else(|| {
            schema
                .url
                .as_deref()
                .and_then(|s| s.strip_prefix("http://hl7.org/fhir/StructureDefinition/"))
        });
        if matches!(typ, Some("date" | "dateTime" | "instant"))
            && value.len() >= 10
            && value
                .get(..10)
                .is_none_or(|date| chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_err())
        {
            findings.push((
                "value",
                path.to_owned(),
                "実在する暦日を指定してください".into(),
            ));
        }
    }
    if schema
        .pattern
        .as_ref()
        .is_some_and(|pattern| crate::profiles::unordered_pattern(v, pattern))
        || schema.slicing.as_ref().is_some_and(|s| {
            s.slices
                .values()
                .filter_map(|s| s.match_.as_ref())
                .filter_map(|m| m.value.as_ref())
                .any(|p| crate::profiles::unordered_pattern(v, p))
        })
    {
        findings.push((
            "not-supported",
            path.to_owned(),
            "複数要素を持つ配列patternの順序を独立して検証できません".into(),
        ));
    }
    if schema.slicing.as_ref().is_some_and(|s| {
        s.slices.values().any(|s| {
            s.match_.is_none()
                && (s.min.is_some_and(|m| m > 0) || could_match(v, s.schema.as_ref()))
        })
    }) {
        findings.push((
            "not-supported",
            path.to_owned(),
            "この要素のslice判定に未対応条件があります".into(),
        ));
    }
    if let Some(values) = v.as_array() {
        for (i, value) in values.iter().enumerate() {
            audit_node(
                value,
                schema,
                &format!("{path}[{i}]"),
                resolver,
                seen,
                findings,
            );
        }
        return;
    }
    // Populated targetProfile requirements are evaluated by Service against
    // resolved resources, with cycle detection, after the structural pass.
    for key in [schema.base.as_ref(), schema.type_.as_ref()]
        .into_iter()
        .flatten()
    {
        let key = if key == "Resource" {
            v["resourceType"].as_str().unwrap_or(key)
        } else {
            key
        };
        if let Some(s) = resolver.resolve(key) {
            audit_node(v, &s, path, resolver, seen, findings);
        }
    }
    if let Some(elements) = &schema.elements {
        for (key, child) in elements {
            if let Some(value) = v.get(key) {
                audit_node(
                    value,
                    child,
                    &format!("{path}.{key}"),
                    resolver,
                    seen,
                    findings,
                );
            }
        }
    }
    if let Some(extensions) = &schema.extensions
        && let Some(values) = v["extension"].as_array()
    {
        for (i, value) in values.iter().enumerate() {
            if let Some(schema) = value["url"].as_str().and_then(|url| extensions.get(url)) {
                audit_node(
                    value,
                    schema,
                    &format!("{path}.extension[{i}]"),
                    resolver,
                    seen,
                    findings,
                );
            }
        }
    }
}

fn could_match(value: &Value, schema: Option<&Arc<FhirSchema>>) -> bool {
    if let Some(items) = value.as_array() {
        return items.iter().any(|v| could_match(v, schema));
    }
    let Some(schema) = schema else {
        return true;
    };
    let resource = schema.elements.as_ref().and_then(|e| e.get("resource"));
    let expected = resource
        .map(|s| s.as_ref())
        .unwrap_or(schema)
        .type_
        .as_deref();
    let actual = if resource.is_some() {
        value["resource"]["resourceType"].as_str()
    } else {
        value["resourceType"].as_str()
    };
    match (expected, actual) {
        (Some(expected), Some(actual)) if !matches!(expected, "Resource" | "DomainResource") => {
            expected == actual
        }
        _ => true,
    }
}

/// Resolve only within the supplied export. Ambiguous logical IDs remain unknown.
pub fn resolve<'a>(root: &'a Value, owner: &'a Value, reference: &str) -> Option<&'a Value> {
    if let Some(id) = reference.strip_prefix('#') {
        return owner["contained"]
            .as_array()?
            .iter()
            .find(|r| r["id"] == id);
    }
    let mut found = None;
    if let (Some(typ), Some(id)) = (root["resourceType"].as_str(), root["id"].as_str())
        && reference == format!("{typ}/{id}")
    {
        found = Some(root);
    }
    for entry in root["entry"].as_array().into_iter().flatten() {
        let resource = &entry["resource"];
        let logical = match (resource["resourceType"].as_str(), resource["id"].as_str()) {
            (Some(typ), Some(id)) => reference == format!("{typ}/{id}"),
            _ => false,
        };
        if entry["fullUrl"] == reference || logical {
            if found.is_some() {
                return None;
            }
            found = Some(resource);
        }
    }
    found
}

fn walk<'a>(
    v: &'a Value,
    owner: &'a Value,
    path: &str,
    index: &HashMap<String, &'a Value>,
    findings: &mut Vec<Finding>,
) {
    match v {
        Value::Object(map) => {
            let owner = if v.get("resourceType").is_some() {
                v
            } else {
                owner
            };
            if let Some(reference) = v["reference"].as_str() {
                let resolved = if let Some(id) = reference.strip_prefix('#') {
                    owner["contained"]
                        .as_array()
                        .and_then(|a| a.iter().find(|r| r["id"] == id))
                } else {
                    index.get(reference).copied()
                };
                if let Some(target) = resolved {
                    if let Some(typ) = v["type"].as_str()
                        && target["resourceType"].as_str() != typ.rsplit('/').next()
                    {
                        findings.push((
                            "structure",
                            format!("{path}.type"),
                            "Reference.typeと参照先の型が一致しません".into(),
                        ));
                    }
                } else {
                    let code = if reference.starts_with('#')
                        || reference.starts_with("urn:uuid:")
                        || reference.starts_with("urn:oid:")
                    {
                        "not-found"
                    } else {
                        "not-supported"
                    };
                    findings.push((
                        code,
                        format!("{path}.reference"),
                        "参照先をローカルで解決できません".into(),
                    ));
                }
            }
            for (name, child) in map {
                walk(child, owner, &format!("{path}.{name}"), index, findings);
            }
        }
        Value::Array(array) => {
            for (i, child) in array.iter().enumerate() {
                walk(child, owner, &format!("{path}[{i}]"), index, findings);
            }
        }
        _ => {}
    }
}
