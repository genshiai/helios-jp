// SPDX-License-Identifier: Apache-2.0
//! Small, explicit corrections around the pinned upstream SD converter.
//! Unsupported discriminators remain unsupported; never infer conformance from meta.profile.
use helios_fhir_validator::{FhirSchema, Match, SchemaResolver};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

pub fn complete(sd: &Value, schema: &mut FhirSchema, warnings: &mut Vec<String>) {
    inherited_choices(schema);
    let Some(elements) = sd["differential"]["element"]
        .as_array()
        .or_else(|| sd["snapshot"]["element"].as_array())
    else {
        return;
    };
    // A slice of a choice is a constraint on the concrete JSON branch. The
    // converter leaves these beneath an unreachable literal `value[x]` key.
    for ed in elements {
        let Some(id) = ed["id"].as_str() else {
            continue;
        };
        let segments: Vec<_> = id.split('.').skip(1).map(str::to_owned).collect();
        let Some((last, parents)) = segments.split_last() else {
            continue;
        };
        let Some((choice, branch)) = last.split_once(':').filter(|(c, _)| c.ends_with("[x]"))
        else {
            continue;
        };
        let Some(parent) = node_at(schema, parents) else {
            continue;
        };
        let constraint = parent
            .elements
            .as_ref()
            .and_then(|e| e.get(choice))
            .and_then(|s| s.slicing.as_ref())
            .and_then(|s| s.slices.get(branch))
            .and_then(|s| s.schema.clone());
        if let Some(constraint) = constraint
            && let Some(target) = parent.elements.as_mut().and_then(|e| e.get_mut(branch))
        {
            let mut combined = serde_json::to_value(target.as_ref()).unwrap();
            merge(
                &mut combined,
                serde_json::to_value(constraint.as_ref()).unwrap(),
            );
            *target = Arc::new(serde_json::from_value(combined).unwrap());
            let start = format!("slice '{branch}': discriminator(s) []");
            warnings.retain(|w| !w.starts_with(&start));
        }
    }
    for ed in elements {
        let Some(id) = ed["id"].as_str() else {
            continue;
        };
        let Some(types) = ed["type"].as_array() else {
            continue;
        };
        for typ in types {
            if typ["profile"]
                .as_array()
                .is_some_and(|profiles| profiles.len() > 1)
            {
                warnings.push(format!(
                    "alternative type.profile constraints are not supported: {id}"
                ));
            }
            let profile = typ["profile"]
                .as_array()
                .filter(|p| p.len() == 1)
                .and_then(|p| p[0].as_str());
            let targets = typ["targetProfile"]
                .as_array()
                .map(|p| {
                    p.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if profile.is_none() && targets.is_empty() {
                continue;
            }
            let segments: Vec<String> = id
                .split('.')
                .skip(1)
                .map(|segment| {
                    if let Some((base, tail)) = segment.split_once("[x]") {
                        if let Some(branch) = tail
                            .strip_prefix(':')
                            .filter(|branch| branch.starts_with(base))
                        {
                            return branch.to_owned();
                        }
                        let code = typ["code"].as_str().unwrap_or("");
                        let mut chars = code.chars();
                        return format!(
                            "{base}{}",
                            chars
                                .next()
                                .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
                                .unwrap_or_default()
                        );
                    }
                    segment.to_owned()
                })
                .collect();
            if let Some(node) = node_at(schema, &segments) {
                if let Some(profile) = profile
                    && node.base.is_none()
                {
                    node.base = Some(profile.to_owned());
                }
                if node.type_.is_none() {
                    node.type_ = typ["code"].as_str().map(str::to_owned);
                }
                if !targets.is_empty() {
                    node.refers = Some(targets);
                }
            }
        }
    }
    // Value discriminators may point at a complex value or include an
    // unconstrained display. Build the matcher from the actual fixed children;
    // all remaining slice constraints are still applied by the engine.
    for ed in elements {
        let Some(ds) = ed["slicing"]["discriminator"]
            .as_array()
            .filter(|ds| !ds.is_empty() && ds.iter().all(|d| d["type"] == "value"))
        else {
            continue;
        };
        let Some(id) = ed["id"].as_str() else {
            continue;
        };
        let segments: Vec<_> = id.split('.').skip(1).map(str::to_owned).collect();
        let Some(slicing) = node_at(schema, &segments).and_then(|s| s.slicing.as_mut()) else {
            continue;
        };
        for (name, slice) in &mut slicing.slices {
            if slice.match_.is_some() {
                continue;
            }
            let Some(child) = &slice.schema else {
                continue;
            };
            let pattern = if ds.len() == 1 && ds[0]["path"] == "$this" {
                fixed_shape(child)
            } else {
                let mut shape = serde_json::Map::new();
                let mut supported = true;
                for d in ds {
                    let Some(path) = d["path"]
                        .as_str()
                        .filter(|p| !p.contains('.') && *p != "$this")
                    else {
                        supported = false;
                        break;
                    };
                    if let Some(node) = child.elements.as_ref().and_then(|e| e.get(path)) {
                        if let Some(fixed) = fixed_shape(node) {
                            shape.insert(path.to_owned(), fixed);
                        } else if node.binding.is_some() || node.constraints.is_some() {
                            supported = false;
                        }
                    }
                }
                if supported && !shape.is_empty() {
                    Some(Value::Object(shape))
                } else {
                    None
                }
            };
            if let Some(pattern) = pattern {
                slice.match_ = Some(Match {
                    type_: Some("pattern".into()),
                    value: Some(pattern),
                    resolve_ref: None,
                });
                warnings.retain(|w| !w.starts_with(&format!("slice '{name}': discriminator(s)")));
            }
        }
    }
    // The converter drops minimums on unsupported matchers; retain the source
    // bounds before an inherited or native matcher makes the slice executable.
    for ed in elements {
        let Some(id) = ed["id"].as_str() else {
            continue;
        };
        let segments: Vec<_> = id.split('.').skip(1).map(str::to_owned).collect();
        if let Some((last, parents)) = segments.split_last()
            && let Some((name, slice)) = last.split_once(':')
            && let Some(parent) = node_at(schema, parents)
            && let Some(slice) = parent
                .elements
                .as_mut()
                .and_then(|e| e.get_mut(name))
                .map(Arc::make_mut)
                .and_then(|s| s.slicing.as_mut())
                .and_then(|s| s.slices.get_mut(slice))
        {
            slice.min = ed["min"].as_u64().or(slice.min);
            if let Some(max) = ed["max"].as_str() {
                slice.max = max.parse::<u64>().ok();
            }
        }
    }
    // Compile distinct resource types independently. Same-type alternative
    // profiles retain their unsupported marker and cannot silently pass.
    for ed in elements {
        let Some(discriminators) = ed["slicing"]["discriminator"].as_array() else {
            continue;
        };
        if discriminators.len() != 1 || discriminators[0]["type"] != "profile" {
            continue;
        }
        let Some(path @ ("resource" | "$this")) = discriminators[0]["path"].as_str() else {
            continue;
        };
        let direct = path == "$this";
        let Some(id) = ed["id"].as_str() else {
            continue;
        };
        let prefix = format!("{id}:");
        let slices: Vec<_> = elements
            .iter()
            .filter(|e| {
                e["id"]
                    .as_str()
                    .is_some_and(|s| s.starts_with(&prefix) && !s[prefix.len()..].contains('.'))
            })
            .collect();
        let mut counts = HashMap::new();
        let mut candidates = Vec::new();
        for slice in slices {
            let slice_id = slice["id"].as_str().unwrap();
            let name = &slice_id[prefix.len()..];
            let resource_id = format!("{slice_id}.resource");
            let resource = if direct {
                slice
            } else {
                match elements.iter().find(|e| e["id"] == resource_id) {
                    Some(r) => r,
                    None => continue,
                }
            };
            let Some(typ) = resource["type"]
                .as_array()
                .filter(|t| t.len() == 1)
                .map(|t| &t[0])
            else {
                continue;
            };
            let Some(code) = typ["code"].as_str() else {
                continue;
            };
            *counts.entry(code.to_owned()).or_insert(0usize) += 1;
            if typ["profile"].as_array().is_some_and(|p| p.len() == 1) {
                candidates.push((
                    name.to_owned(),
                    code.to_owned(),
                    slice["min"].as_u64(),
                    slice["max"].as_str().and_then(|s| s.parse::<u64>().ok()),
                ));
            }
        }
        let segments: Vec<_> = id.split('.').skip(1).map(str::to_owned).collect();
        let Some(node) = node_at(schema, &segments) else {
            continue;
        };
        let Some(slicing) = node.slicing.as_mut() else {
            continue;
        };
        for (name, code, min, max) in candidates {
            if counts[&code] != 1 || matches!(code.as_str(), "Resource" | "DomainResource") {
                continue;
            }
            let Some(slice) = slicing.slices.get_mut(&name) else {
                continue;
            };
            let layered = slice.schema.as_ref().is_some_and(|s| {
                if direct {
                    s.base.is_some()
                } else {
                    s.elements
                        .as_ref()
                        .and_then(|e| e.get("resource"))
                        .is_some_and(|r| r.base.is_some())
                }
            });
            if !layered {
                continue;
            }
            slice.match_ = Some(Match {
                type_: Some("pattern".into()),
                value: Some(if direct {
                    json!({"resourceType":code})
                } else {
                    json!({"resource":{"resourceType":code}})
                }),
                resolve_ref: None,
            });
            slice.min = min;
            slice.max = max;
            let start = format!("slice '{name}': discriminator(s) [\"profile@{path}\"]");
            warnings.retain(|w| !w.starts_with(&start));
        }
    }
}

fn merge(into: &mut Value, other: Value) {
    if let (Some(into), Some(other)) = (into.as_object_mut(), other.as_object()) {
        for (key, value) in other {
            if key == "required" {
                let current = into.entry(key.clone()).or_insert_with(|| json!([]));
                if let (Some(a), Some(b)) = (current.as_array_mut(), value.as_array()) {
                    for v in b {
                        if !a.contains(v) {
                            a.push(v.clone());
                        }
                    }
                }
            } else if let Some(current) = into.get_mut(key) {
                merge(current, value.clone());
            } else {
                into.insert(key.clone(), value.clone());
            }
        }
    } else {
        *into = other;
    }
}

fn fixed_shape(schema: &FhirSchema) -> Option<Value> {
    if let Some(value) = schema.fixed.clone().or_else(|| schema.pattern.clone()) {
        return Some(value);
    }
    let mut shape = serde_json::Map::new();
    if let Some(elements) = &schema.elements {
        for (name, child) in elements {
            if let Some(value) = fixed_shape(child) {
                shape.insert(name.clone(), value);
            }
        }
    }
    (!shape.is_empty()).then_some(Value::Object(shape))
}

/// A differential may constrain a named slice without repeating its discriminator.
/// Reuse the base matcher and retain the derived slice's own constraints/bounds.
pub fn inherit(schema: &mut FhirSchema, resolver: &dyn SchemaResolver, warnings: &mut Vec<String>) {
    inherit_node(schema, &[], resolver, warnings);
}

fn inherit_node(
    schema: &mut FhirSchema,
    inherited: &[Arc<FhirSchema>],
    resolver: &dyn SchemaResolver,
    warnings: &mut Vec<String>,
) {
    let mut bases = inherited.to_vec();
    if let Some(reference) = &schema.element_reference
        && let Some(base) = element_reference(reference, resolver)
    {
        bases.push(base);
    }
    for key in [schema.base.as_ref(), schema.type_.as_ref()]
        .into_iter()
        .flatten()
    {
        if let Some(base) = resolver.resolve(key) {
            bases.push(base);
        }
    }
    let mut seen = HashSet::new();
    let mut cursor = 0;
    while cursor < bases.len() && bases.len() < 100 {
        let base = bases[cursor].clone();
        cursor += 1;
        if !seen.insert(Arc::as_ptr(&base) as usize) {
            continue;
        }
        if let Some(reference) = &base.element_reference
            && let Some(parent) = element_reference(reference, resolver)
            && !seen.contains(&(Arc::as_ptr(&parent) as usize))
        {
            bases.push(parent);
        }
        for key in [base.base.as_ref(), base.type_.as_ref()]
            .into_iter()
            .flatten()
        {
            if let Some(parent) = resolver.resolve(key)
                && !seen.contains(&(Arc::as_ptr(&parent) as usize))
            {
                bases.push(parent);
            }
        }
    }
    let own = Arc::new(schema.clone());
    if let Some(slicing) = schema.slicing.as_mut() {
        for (name, slice) in &mut slicing.slices {
            let warning = format!("slice '{name}': discriminator(s) []");
            if slice.match_.is_none() && warnings.iter().any(|w| w.starts_with(&warning)) {
                if let Some(parent) = bases
                    .iter()
                    .filter_map(|s| s.slicing.as_ref())
                    .filter_map(|s| s.slices.get(name))
                    .find(|s| s.match_.is_some())
                {
                    slice.match_ = parent.match_.clone();
                    slice.order = slice.order.or(parent.order);
                    warnings.retain(|w| !w.starts_with(&warning));
                } else {
                    // A new Identifier.system slice can inherit the exact
                    // discriminator shape demonstrated by the base slices.
                    let system_discriminator =
                        bases.iter().filter_map(|b| b.slicing.as_ref()).any(|s| {
                            !s.slices.is_empty()
                                && s.slices.values().all(|s| {
                                    s.match_.as_ref().is_some_and(|m| {
                                        m.type_.as_deref() == Some("pattern")
                                            && m.value
                                                .as_ref()
                                                .and_then(Value::as_object)
                                                .is_some_and(|v| {
                                                    v.len() == 1 && v.contains_key("system")
                                                })
                                    })
                                })
                        });
                    let constant = slice
                        .schema
                        .as_ref()
                        .and_then(|s| s.elements.as_ref())
                        .and_then(|e| e.get("system"))
                        .and_then(|s| s.fixed.clone().or_else(|| s.pattern.clone()));
                    if system_discriminator && let Some(system) = constant {
                        slice.match_ = Some(Match {
                            type_: Some("pattern".into()),
                            value: Some(json!({"system":system})),
                            resolve_ref: None,
                        });
                        warnings.retain(|w| !w.starts_with(&warning));
                    }
                }
            }
            if let Some(value) = slice
                .match_
                .as_mut()
                .filter(|m| m.type_.as_deref() == Some("pattern"))
                .and_then(|m| m.value.as_mut())
            {
                let mut shapes = bases.clone();
                shapes.push(own.clone());
                if let Some(child) = &slice.schema {
                    shapes.push(child.clone());
                }
                normalize_pattern(value, &shapes, resolver);
            }
            if let Some(child) = slice.schema.as_mut() {
                let mut inherited = bases.clone();
                inherited.push(own.clone());
                inherited.extend(
                    bases
                        .iter()
                        .filter_map(|s| s.slicing.as_ref())
                        .filter_map(|s| s.slices.get(name))
                        .filter_map(|s| s.schema.clone()),
                );
                inherit_node(Arc::make_mut(child), &inherited, resolver, warnings);
            }
        }
    }
    if let Some(elements) = schema.elements.as_mut() {
        for (name, child) in elements {
            let mut inherited: Vec<_> = bases
                .iter()
                .filter_map(|s| s.elements.as_ref())
                .filter_map(|e| e.get(name))
                .cloned()
                .collect();
            if name == "extension" {
                for base in bases.iter().chain(std::iter::once(&own)) {
                    let Some(extensions) = &base.extensions else {
                        continue;
                    };
                    let mut slices = serde_json::Map::new();
                    for (name, extension) in extensions {
                        let Some(url) = extension.url.as_ref() else {
                            continue;
                        };
                        slices.insert(name.clone(),json!({"match":{"type":"pattern","value":{"url":url}},"schema":extension}));
                    }
                    if !slices.is_empty() {
                        inherited.push(Arc::new(
                            serde_json::from_value(json!({"slicing":{"slices":slices}}))
                                .expect("schema extension matcher"),
                        ));
                    }
                }
            }
            inherit_node(Arc::make_mut(child), &inherited, resolver, warnings);
            if child.slicing.as_ref().is_some_and(|s| {
                s.slices
                    .values()
                    .any(|slice| slice.min.is_some_and(|min| min > 0))
            }) {
                let required = schema.required.get_or_insert_with(Vec::new);
                if !required.contains(name) {
                    required.push(name.clone());
                }
            }
        }
    }
}

fn normalize_pattern(
    pattern: &mut Value,
    schemas: &[Arc<FhirSchema>],
    resolver: &dyn SchemaResolver,
) {
    let mut shapes = schemas.to_vec();
    let mut seen = HashSet::new();
    let mut cursor = 0;
    while cursor < shapes.len() && shapes.len() < 100 {
        let schema = shapes[cursor].clone();
        cursor += 1;
        if !seen.insert(Arc::as_ptr(&schema) as usize) {
            continue;
        }
        if let Some(reference) = &schema.element_reference
            && let Some(base) = element_reference(reference, resolver)
            && !seen.contains(&(Arc::as_ptr(&base) as usize))
        {
            shapes.push(base);
        }
        for key in [schema.base.as_ref(), schema.type_.as_ref()]
            .into_iter()
            .flatten()
        {
            if let Some(base) = resolver.resolve(key)
                && !seen.contains(&(Arc::as_ptr(&base) as usize))
            {
                shapes.push(base);
            }
        }
    }
    if let Some(fields) = pattern.as_object_mut() {
        for (name, value) in fields {
            let children: Vec<_> = shapes
                .iter()
                .filter_map(|s| s.elements.as_ref())
                .filter_map(|e| e.get(name))
                .cloned()
                .collect();
            if children.iter().any(|s| s.array == Some(true)) && !value.is_array() {
                *value = Value::Array(vec![value.clone()]);
            }
            if let Some(items) = value.as_array_mut() {
                for item in items {
                    normalize_pattern(item, &children, resolver);
                }
            } else {
                normalize_pattern(value, &children, resolver);
            }
        }
    }
}

fn element_reference(path: &[String], resolver: &dyn SchemaResolver) -> Option<Arc<FhirSchema>> {
    let (root, tail) = path.split_first()?;
    let mut node = resolver.resolve(root)?;
    let mut segments = tail.iter();
    while let Some(segment) = segments.next() {
        if segment != "elements" {
            return None;
        }
        node = node.elements.as_ref()?.get(segments.next()?)?.clone();
    }
    Some(node)
}

pub fn unordered_pattern(value: &Value, pattern: &Value) -> bool {
    match (value, pattern) {
        (Value::Array(values), Value::Array(patterns)) => {
            values.len() > 1
                || patterns.len() > 1
                || values
                    .iter()
                    .zip(patterns)
                    .any(|(v, p)| unordered_pattern(v, p))
        }
        (Value::Object(values), Value::Object(patterns)) => patterns
            .iter()
            .any(|(k, p)| values.get(k).is_some_and(|v| unordered_pattern(v, p))),
        _ => false,
    }
}

fn node_at<'a>(node: &'a mut FhirSchema, segments: &[String]) -> Option<&'a mut FhirSchema> {
    let Some((head, tail)) = segments.split_first() else {
        return Some(node);
    };
    let (name, slice) = head
        .split_once(':')
        .map_or((head.as_str(), None), |(name, slice)| (name, Some(slice)));
    let child = Arc::make_mut(node.elements.as_mut()?.get_mut(name)?);
    let child = match slice {
        Some(slice) => Arc::make_mut(
            child
                .slicing
                .as_mut()?
                .slices
                .get_mut(slice)?
                .schema
                .get_or_insert_with(|| Arc::new(FhirSchema::default())),
        ),
        None => child,
    };
    node_at(child, tail)
}

fn inherited_choices(schema: &mut FhirSchema) {
    // An omitted type list in a differential inherits the base choices; the
    // upstream converter emits choices: [], which would prohibit all branches.
    if schema.choices.as_ref().is_some_and(Vec::is_empty) {
        schema.choices = None;
    }
    for children in [&mut schema.elements, &mut schema.extensions]
        .into_iter()
        .flatten()
    {
        for child in children.values_mut() {
            inherited_choices(Arc::make_mut(child));
        }
    }
    if let Some(slicing) = schema.slicing.as_mut() {
        for slice in slicing.slices.values_mut() {
            if let Some(child) = slice.schema.as_mut() {
                inherited_choices(Arc::make_mut(child));
            }
        }
    }
}

/// Profiles applied by an enclosing element/slice also apply when its embedded
/// resource is evaluated independently for FHIRPath and terminology.
pub fn applied_children(
    root: &Value,
    resolver: &dyn SchemaResolver,
    keys: &[String],
) -> HashMap<usize, Vec<String>> {
    inspect(root, resolver, keys).profiles
}

pub struct ReferenceRequirement<'a> {
    pub owner: &'a Value,
    pub reference: &'a str,
    pub profiles: Vec<String>,
    pub path: String,
}

#[derive(Default)]
pub struct Inspection<'a> {
    pub profiles: HashMap<usize, Vec<String>>,
    pub references: Vec<ReferenceRequirement<'a>>,
}

pub fn inspect<'a>(
    root: &'a Value,
    resolver: &dyn SchemaResolver,
    keys: &[String],
) -> Inspection<'a> {
    let mut output = Inspection::default();
    let mut seen = HashSet::new();
    for key in keys {
        if let Some(schema) = resolver.resolve(key) {
            scan(
                root,
                root,
                &schema,
                root["resourceType"].as_str().unwrap_or("Resource"),
                resolver,
                &mut seen,
                &mut output,
            );
        }
    }
    output
}

#[allow(clippy::too_many_arguments)]
fn scan<'a>(
    value: &'a Value,
    owner: &'a Value,
    schema: &Arc<FhirSchema>,
    path: &str,
    resolver: &dyn SchemaResolver,
    seen: &mut HashSet<(usize, usize)>,
    output: &mut Inspection<'a>,
) {
    if !seen.insert((value as *const Value as usize, Arc::as_ptr(schema) as usize)) {
        return;
    }
    if let Some(items) = value.as_array() {
        for (i, item) in items.iter().enumerate() {
            scan(
                item,
                owner,
                schema,
                &format!("{path}[{i}]"),
                resolver,
                seen,
                output,
            );
        }
        return;
    }
    let owner = if value.get("resourceType").is_some() {
        value
    } else {
        owner
    };
    if let (Some(profiles), Some(reference)) = (&schema.refers, value["reference"].as_str())
        && !profiles.is_empty()
    {
        output.references.push(ReferenceRequirement {
            owner,
            reference,
            profiles: profiles.clone(),
            path: path.to_owned(),
        });
    }
    if value.get("resourceType").is_some()
        && schema.kind.as_deref() == Some("resource")
        && let Some(url) = &schema.url
    {
        let profiles = output
            .profiles
            .entry(value as *const Value as usize)
            .or_default();
        if !profiles.contains(url) {
            profiles.push(url.clone());
        }
    }
    for key in [schema.base.as_ref(), schema.type_.as_ref()]
        .into_iter()
        .flatten()
    {
        let key = if key == "Resource" {
            value["resourceType"].as_str().unwrap_or(key)
        } else {
            key
        };
        if let Some(parent) = resolver.resolve(key) {
            scan(value, owner, &parent, path, resolver, seen, output);
        }
    }
    if let Some(elements) = &schema.elements {
        for (key, child) in elements {
            if let Some(value) = value.get(key) {
                scan(
                    value,
                    owner,
                    child,
                    &format!("{path}.{key}"),
                    resolver,
                    seen,
                    output,
                );
            }
        }
    }
    if let Some(slicing) = &schema.slicing {
        for slice in slicing.slices.values() {
            if slice
                .match_
                .as_ref()
                .and_then(|m| m.value.as_ref())
                .is_some_and(|p| partial_match(value, p))
                && let Some(child) = &slice.schema
            {
                scan(value, owner, child, path, resolver, seen, output);
            }
        }
    }
}

pub fn partial_match(value: &Value, pattern: &Value) -> bool {
    match (value, pattern) {
        (Value::Object(value), Value::Object(pattern)) => pattern
            .iter()
            .all(|(k, p)| value.get(k).is_some_and(|v| partial_match(v, p))),
        (Value::Array(value), Value::Array(pattern)) => pattern
            .iter()
            .all(|p| value.iter().any(|v| partial_match(v, p))),
        _ => value == pattern,
    }
}

#[derive(Clone)]
pub struct WarningCondition {
    path: String,
    minimum: u64,
    resource_type: Option<String>,
    in_resource: bool,
}
pub type WarningConditions = HashMap<String, Vec<WarningCondition>>;

pub fn conditional_warnings(sd: &Value, warnings: &[String]) -> WarningConditions {
    let mut output = HashMap::new();
    if sd["kind"] != "resource" {
        return output;
    }
    let Some(elements) = sd["differential"]["element"]
        .as_array()
        .or_else(|| sd["snapshot"]["element"].as_array())
    else {
        return output;
    };
    for warning in warnings {
        let Some((name, _)) = warning
            .strip_prefix("slice '")
            .and_then(|w| w.split_once("':"))
        else {
            continue;
        };
        let mut conditions = Vec::new();
        for ed in elements.iter().filter(|e| e["sliceName"] == name) {
            let Some(path) = ed["path"].as_str() else {
                continue;
            };
            let parent = ed["id"]
                .as_str()
                .and_then(|s| s.rsplit_once(':'))
                .map(|s| s.0);
            let discriminator = parent
                .and_then(|p| elements.iter().find(|e| e["id"] == p))
                .and_then(|e| e["slicing"]["discriminator"].as_array())
                .filter(|d| d.len() == 1)
                .map(|d| &d[0]);
            let resource_path = discriminator
                .filter(|d| d["type"] == "profile")
                .and_then(|d| d["path"].as_str());
            let in_resource = resource_path == Some("resource");
            let typed = match resource_path {
                Some("$this") => Some(ed),
                Some("resource") => ed["id"].as_str().and_then(|id| {
                    elements
                        .iter()
                        .find(|e| e["id"] == format!("{id}.resource"))
                }),
                _ => None,
            };
            let resource_type = typed
                .and_then(|e| e["type"].as_array())
                .filter(|t| t.len() == 1)
                .and_then(|t| t[0]["code"].as_str())
                .filter(|t| !matches!(*t, "Resource" | "DomainResource" | "Reference"))
                .map(str::to_owned);
            conditions.push(WarningCondition {
                path: path.to_owned(),
                minimum: ed["min"].as_u64().unwrap_or(0),
                resource_type,
                in_resource,
            });
        }
        if !conditions.is_empty() {
            output.insert(warning.clone(), conditions);
        }
    }
    output
}

pub fn warning_active(resource: &Value, conditions: Option<&Vec<WarningCondition>>) -> bool {
    conditions.is_none_or(|conditions| {
        conditions.iter().any(|c| {
            c.minimum > 0 || populated(resource, &c.path.split('.').skip(1).collect::<Vec<_>>(), c)
        })
    })
}

fn populated(value: &Value, path: &[&str], condition: &WarningCondition) -> bool {
    if let Some(items) = value.as_array() {
        return items.iter().any(|v| populated(v, path, condition));
    }
    let Some((name, tail)) = path.split_first() else {
        if value.is_null() {
            return false;
        }
        let Some(expected) = &condition.resource_type else {
            return true;
        };
        let resource = if condition.in_resource {
            &value["resource"]
        } else {
            value
        };
        return resource["resourceType"]
            .as_str()
            .is_none_or(|actual| actual == expected);
    };
    if let Some(prefix) = name.strip_suffix("[x]") {
        return value.as_object().is_some_and(|m| {
            m.iter().any(|(name, v)| {
                name.starts_with(prefix)
                    && name.len() > prefix.len()
                    && populated(v, tail, condition)
            })
        });
    }
    value
        .get(*name)
        .is_some_and(|v| populated(v, tail, condition))
}

/// Reference profile slices are supported when an explicit Reference.type
/// selects a distinct target resource type. The target's full profile is
/// validated separately; a type label alone never establishes conformance.
pub fn resolved_reference_slices(
    schema: &mut FhirSchema,
    types: &HashMap<String, String>,
    warnings: &mut Vec<String>,
) {
    if let Some(slicing) = schema.slicing.as_mut() {
        let mut candidates = Vec::new();
        let mut counts = HashMap::new();
        for (name, slice) in &slicing.slices {
            if slice.match_.is_some() {
                continue;
            }
            let eligible = ["resolve()", "$this.resolve()"].iter().any(|path| {
                warnings.iter().any(|w| {
                    w.starts_with(&format!(
                        "slice '{name}': discriminator(s) [\"profile@{path}\"]"
                    ))
                })
            });
            if !eligible {
                continue;
            }
            let Some(profiles) = slice
                .schema
                .as_ref()
                .and_then(|s| s.refers.as_ref())
                .filter(|p| !p.is_empty())
            else {
                continue;
            };
            let targets: Option<HashSet<_>> = profiles
                .iter()
                .map(|p| {
                    let url = p.split('|').next().unwrap_or(p);
                    types.get(url).cloned().or_else(|| {
                        url.strip_prefix("http://hl7.org/fhir/StructureDefinition/")
                            .map(str::to_owned)
                    })
                })
                .collect();
            let Some(targets) = targets.filter(|t| t.len() == 1) else {
                continue;
            };
            let target = targets.into_iter().next().unwrap();
            if matches!(target.as_str(), "Resource" | "DomainResource") {
                continue;
            }
            *counts.entry(target.clone()).or_insert(0usize) += 1;
            candidates.push((name.clone(), target));
        }
        for (name, target) in candidates {
            if counts[&target] != 1 {
                continue;
            }
            let slice = slicing.slices.get_mut(&name).unwrap();
            slice.match_ = Some(Match {
                type_: Some("pattern".into()),
                value: Some(json!({"type":target})),
                resolve_ref: None,
            });
            warnings.retain(|w| {
                !["resolve()", "$this.resolve()"].iter().any(|path| {
                    w.starts_with(&format!(
                        "slice '{name}': discriminator(s) [\"profile@{path}\"]"
                    ))
                })
            });
        }
        for slice in slicing.slices.values_mut() {
            if let Some(child) = slice.schema.as_mut() {
                resolved_reference_slices(Arc::make_mut(child), types, warnings);
            }
        }
    }
    for children in [&mut schema.elements, &mut schema.extensions]
        .into_iter()
        .flatten()
    {
        for child in children.values_mut() {
            resolved_reference_slices(Arc::make_mut(child), types, warnings);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compiles_distinct_contained_resource_profile_slices() {
        let sd = json!({"resourceType":"StructureDefinition","url":"urn:profile","name":"TestCondition","version":"1","fhirVersion":"4.0.1","kind":"resource","type":"Condition","derivation":"constraint","baseDefinition":"http://hl7.org/fhir/StructureDefinition/Condition","differential":{"element":[
            {"id":"Condition","path":"Condition"},
            {"id":"Condition.contained","path":"Condition.contained","slicing":{"discriminator":[{"type":"profile","path":"$this"}],"rules":"open"}},
            {"id":"Condition.contained:encounter","path":"Condition.contained","sliceName":"encounter","min":0,"max":"1","type":[{"code":"Encounter","profile":["urn:Encounter"]}]},
            {"id":"Condition.contained:recorder","path":"Condition.contained","sliceName":"recorder","min":0,"max":"1","type":[{"code":"Practitioner","profile":["urn:Practitioner"]}]}
        ]}});
        let mut result = helios_fhir_validator::converter::convert(&sd).unwrap();
        complete(&sd, &mut result.schema, &mut result.warnings);
        let slicing = result.schema.elements.as_ref().unwrap()["contained"]
            .slicing
            .as_ref()
            .unwrap();
        assert!(
            slicing.slices.values().all(|s| s.match_.is_some()),
            "{slicing:?}; {:?}",
            result.warnings
        );
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    }
}
