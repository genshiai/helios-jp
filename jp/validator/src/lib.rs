// SPDX-License-Identifier: Apache-2.0
//! Offline native Rust FHIR validation. Unavailable rules remain incomplete.
mod invariants;
mod narrative;
pub mod packages;
mod profiles;
mod references;
mod terminology;
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::Arc,
};

use anyhow::{Context, Result, ensure};
use helios_fhir::FhirVersion;
use helios_fhir_validator::{
    CompositeResolver, ConstraintEvaluator, ConstraintOutcome, Deferred, DeferredConstraint,
    ErrorKind, SchemaRegistry, SchemaResolver, Severity, UnknownProfilePolicy, ValidationOptions,
    Validator, converter, packs,
};
use serde::Serialize;
use serde_json::{Value, json};

pub const MAX_INPUT_BYTES: usize = 2 * 1024 * 1024;
pub const ENGINE: &str = "helios-fhir-validator/0.2.7";

#[derive(Clone, Serialize)]
pub struct Definition {
    pub url: String,
    pub version: String,
    pub conversion_warnings: Vec<String>,
    #[serde(skip)]
    conditional_warnings: profiles::WarningConditions,
}

#[derive(Serialize)]
pub struct Report {
    /// invalid = observed rule violation; incomplete = no complete judgment yet.
    pub status: &'static str,
    pub fhir_version: &'static str,
    pub engine: &'static str,
    pub structural_checks_passed: bool,
    pub deferred_checks: usize,
    pub definitions: Vec<Definition>,
    pub packages: Vec<packages::Package>,
    pub outcome: Value,
}

pub struct Service {
    resource_types: HashMap<String, String>,
    validator: Validator,
    resolver: Arc<dyn SchemaResolver>,
    pub definitions: Vec<Definition>,
    pub packages: Vec<packages::Package>,
    terminology: terminology::Terminology,
    evaluator: helios_fhir_validator::fhirpath_effects::FhirPathConstraintEvaluator,
}

impl Service {
    /// Inspect the compiled FHIR Schema (not a JSON Schema or a patient resource).
    pub fn compiled_schema(&self, canonical: &str) -> Option<Value> {
        self.resolver
            .resolve(canonical)
            .and_then(|s| serde_json::to_value(s.as_ref()).ok())
    }
    /// Read only operator-supplied local definitions. Never fetch user URLs.
    pub fn load(paths: &[impl AsRef<Path>]) -> Result<Self> {
        let mut resources = Vec::new();
        for path in paths {
            let file = std::fs::File::open(path.as_ref()).context("cannot open definition")?;
            ensure!(
                file.metadata()?.len() <= MAX_INPUT_BYTES as u64,
                "definition too large"
            );
            resources.push(serde_json::from_reader(file).context("invalid definition JSON")?);
        }
        Self::from_resources(resources, Vec::new())
    }

    pub fn from_lock(path: &Path) -> Result<Self> {
        let (packages, resources) = packages::load(path)?;
        Self::from_resources(resources, packages)
    }

    fn from_resources(resources: Vec<Value>, packages: Vec<packages::Package>) -> Result<Self> {
        let core = packs::core_registry(FhirVersion::R4);
        let mut profiles = SchemaRegistry::new();
        let mut definitions = Vec::new();
        let mut seen = HashSet::new();
        let mut terminology = terminology::Terminology::default();
        let mut resource_types = HashMap::new();
        let mut pending_schemas = Vec::new();
        let mut core_aliases = Vec::new();
        for sd in resources {
            if sd["resourceType"] != "StructureDefinition" {
                terminology.insert(sd);
                continue;
            }
            let url = sd["url"]
                .as_str()
                .context("definition requires canonical url")?
                .to_owned();
            if sd["kind"] == "resource"
                && let Some(typ) = sd["type"].as_str()
            {
                resource_types.insert(url.clone(), typ.to_owned());
            }
            ensure!(
                seen.insert(url.clone()),
                "duplicate definition canonical: {url}"
            );
            if let Some(schema) = core.resolve(&url) {
                ensure!(
                    !packages.is_empty(),
                    "cannot replace core definition: {url}"
                );
                if sd["version"] == "4.0.1"
                    && url.starts_with("http://hl7.org/fhir/StructureDefinition/")
                {
                    let key = format!("{url}|4.0.1");
                    profiles.insert_named(&key, schema.as_ref().clone());
                    core_aliases.push((key, schema.as_ref().clone()));
                }
                continue;
            }
            ensure!(
                sd["fhirVersion"].as_str().is_none_or(|v| v == "4.0.1")
                    || (sd["fhirVersion"] == "4.0.0"
                        && !packages.is_empty()
                        && [
                            "structuredefinition-xml-type",
                            "structuredefinition-json-type",
                            "structuredefinition-rdf-type"
                        ]
                        .iter()
                        .any(
                            |name| url == format!("http://hl7.org/fhir/StructureDefinition/{name}")
                        )),
                "definition must target R4 4.0.1"
            );
            let version = sd["version"]
                .as_str()
                .context("definition requires version")?
                .to_owned();
            let mut converted = match converter::convert(&sd) {
                Ok(converted) => converted,
                Err(error) if !packages.is_empty() => {
                    definitions.push(Definition {
                        url,
                        version,
                        conversion_warnings: vec![format!(
                            "definition conversion unavailable: {error}"
                        )],
                        conditional_warnings: HashMap::new(),
                    });
                    continue;
                }
                Err(error) => return Err(error).with_context(|| format!("cannot convert {url}")),
            };
            profiles::complete(&sd, &mut converted.schema, &mut converted.warnings);
            let key = format!("{url}|{version}");
            profiles.insert_named(&key, converted.schema.clone());
            pending_schemas.push((key, converted.schema, definitions.len()));
            definitions.push(Definition {
                url,
                version,
                conditional_warnings: profiles::conditional_warnings(&sd, &converted.warnings),
                conversion_warnings: converted.warnings,
            });
        }
        let initial = CompositeResolver::new(vec![core.clone(), Arc::new(profiles)]);
        let mut profiles = SchemaRegistry::new();
        for (key, mut schema, index) in pending_schemas {
            profiles::inherit(
                &mut schema,
                &initial,
                &mut definitions[index].conversion_warnings,
            );
            profiles::resolved_reference_slices(
                &mut schema,
                &resource_types,
                &mut definitions[index].conversion_warnings,
            );
            profiles.insert_named(key, schema);
        }
        for (key, schema) in core_aliases {
            profiles.insert_named(key, schema);
        }
        // Core names must not be shadowed by profile names (e.g. Patient).
        let resolver: Arc<dyn SchemaResolver> =
            Arc::new(CompositeResolver::new(vec![core, Arc::new(profiles)]));
        Ok(Self {
            resource_types,
            validator: Validator::new(Arc::clone(&resolver)),
            resolver,
            definitions,
            packages,
            terminology,
            evaluator: helios_fhir_validator::fhirpath_effects::FhirPathConstraintEvaluator::new(),
        })
    }

    pub fn validate(&self, resource: &Value, profile: Option<String>) -> Report {
        let mut report = self.validate_inner(resource, profile.clone(), true, &[]);
        let findings = self.reference_targets(resource, profile);
        for finding in findings {
            if finding["severity"] == "error" {
                report.status = "invalid";
            } else {
                report.deferred_checks += 1;
                if report.status != "invalid" {
                    report.status = "incomplete";
                }
            }
            report.structural_checks_passed = false;
            report.outcome["issue"]
                .as_array_mut()
                .expect("outcome issue array")
                .push(finding);
        }
        report
    }

    fn reference_targets(&self, root: &Value, profile: Option<String>) -> Vec<Value> {
        let mut issues = Vec::new();
        let mut pending = vec![(root, profile.into_iter().collect::<Vec<_>>())];
        let mut visited = HashSet::new();
        let mut checked: HashMap<(usize, String), &'static str> = HashMap::new();
        while let Some((resource, mut keys)) = pending.pop() {
            if visited.len() + checked.len() > 1000 {
                issues.push(issue(
                    "warning",
                    "not-supported",
                    "参照先プロファイルの検証上限に達しました",
                    "Resource",
                ));
                break;
            }
            if let Some(meta) = resource["meta"]["profile"].as_array() {
                keys.extend(meta.iter().filter_map(Value::as_str).map(str::to_owned));
            }
            if let Some(typ) = resource["resourceType"].as_str() {
                keys.push(typ.to_owned());
            }
            keys.sort();
            keys.dedup();
            if !visited.insert((resource as *const Value as usize, keys.clone())) {
                continue;
            }
            let inspection = profiles::inspect(resource, self.resolver.as_ref(), &keys);
            for (_, child) in child_resources(resource) {
                pending.push((
                    child,
                    inspection
                        .profiles
                        .get(&(child as *const Value as usize))
                        .cloned()
                        .unwrap_or_default(),
                ));
            }
            for requirement in inspection.references {
                let Some(target) =
                    references::resolve(root, requirement.owner, requirement.reference)
                else {
                    issues.push(issue(
                        "warning",
                        "not-supported",
                        "参照先プロファイルをローカルで検証できません",
                        &requirement.path,
                    ));
                    continue;
                };
                let mut accepted = None;
                let mut unknown = false;
                for profile in requirement.profiles {
                    let key = (target as *const Value as usize, profile.clone());
                    let status = if let Some(status) = checked.get(&key) {
                        *status
                    } else {
                        let status = match self.profile_type(&profile) {
                            Some(typ)
                                if !type_matches(
                                    &typ,
                                    target["resourceType"].as_str().unwrap_or(""),
                                ) =>
                            {
                                "invalid"
                            }
                            None => "incomplete",
                            Some(_) => {
                                self.validate_inner(target, Some(profile.clone()), false, &[])
                                    .status
                            }
                        };
                        checked.insert(key, status);
                        status
                    };
                    match status {
                        "valid" => {
                            accepted = Some(profile);
                            break;
                        }
                        "incomplete" => unknown = true,
                        _ => {}
                    }
                }
                if let Some(profile) = accepted {
                    pending.push((target, vec![profile]));
                } else if unknown {
                    issues.push(issue(
                        "warning",
                        "not-supported",
                        "参照先のtargetProfile検証を完了できません",
                        &requirement.path,
                    ));
                } else {
                    issues.push(issue(
                        "error",
                        "structure",
                        "参照先が許可されたtargetProfileに適合しません",
                        &requirement.path,
                    ));
                }
            }
        }
        issues
    }

    fn profile_type(&self, profile: &str) -> Option<String> {
        let schema = self.resolver.resolve(profile)?;
        let url = schema.url.as_deref().unwrap_or(profile).split('|').next()?;
        self.resource_types.get(url).cloned().or_else(|| {
            url.strip_prefix("http://hl7.org/fhir/StructureDefinition/")
                .filter(|_| schema.kind.as_deref() == Some("resource"))
                .map(str::to_owned)
        })
    }

    fn validate_inner(
        &self,
        resource: &Value,
        profile: Option<String>,
        check_references: bool,
        extra_profiles: &[String],
    ) -> Report {
        let mut issues = Vec::new();
        // Installing the CLINS package selects CLINS validation by default;
        // omitting meta.profile must not silently downgrade to base R4.
        let profile = profile.or_else(|| {
            if !self.packages.iter().any(|p| p.name == "jp-eCSCLINS.r4") {
                return None;
            }
            let has_profile = !extra_profiles.is_empty()
                || resource["meta"]["profile"]
                    .as_array()
                    .is_some_and(|profiles| !profiles.is_empty());
            if has_profile {
                return None;
            }
            let name = match resource["resourceType"].as_str()? {
                "Patient" => "JP_Patient_eCS",
                "Condition" => "JP_Condition_eCS",
                "MedicationRequest" => "JP_MedicationRequest_eCS",
                "Observation" => "JP_Observation_LabResult_eCS",
                "AllergyIntolerance" => "JP_AllergyIntolerance_eCS",
                "Encounter" => "JP_Encounter_eCS",
                "Organization" => "JP_Organization_eCS",
                "Practitioner" => "JP_Practitioner_eCS",
                "Bundle" if resource["type"] == "collection" => {
                    return Some(
                        "http://jpfhir.jp/fhir/clins/StructureDefinition/JP_Bundle_CLINS".into(),
                    );
                }
                _ => return None,
            };
            Some(format!(
                "http://jpfhir.jp/fhir/eCS/StructureDefinition/{name}"
            ))
        });
        let mut invalid = false;
        let mut incomplete = false;
        let mut deferred_checks = 0;
        if self.packages.iter().any(|p| p.name == "jp-eCSCLINS.r4")
            && resource["resourceType"] == "Bundle"
            && resource["type"] == "document"
            && profile.is_none()
        {
            let explicit = resource["meta"]["profile"].as_array().is_some_and(|a| {
                a.iter().filter_map(Value::as_str).any(|p| {
                    p.contains("JP_Bundle_eReferral")
                        || p.contains("JP_Bundle_eDischargeSummary")
                        || p.contains("JP_Bundle_ePCS")
                })
            });
            if !explicit {
                incomplete = true;
                deferred_checks += 1;
                issues.push(issue(
                    "warning",
                    "not-supported",
                    "CLINS文書の種類をprofileで指定してください",
                    "Bundle.meta.profile",
                ));
            }
        }
        let mut used_profiles = HashSet::new();
        collect_profiles(resource, &mut used_profiles);
        used_profiles.extend(extra_profiles.iter().cloned());
        if let Some(p) = &profile {
            used_profiles.insert(p.clone());
        }
        let mut pending: Vec<_> = used_profiles.iter().cloned().collect();
        while let Some(p) = pending.pop() {
            if let Some(schema) = self.resolver.resolve(&p)
                && let Some(base) = &schema.base
                && used_profiles.insert(base.clone())
            {
                pending.push(base.clone());
            }
        }
        let mut root_profiles = extra_profiles.to_vec();
        if let Some(profile) = &profile {
            root_profiles.push(profile.clone());
        }
        if let Some(profiles) = resource["meta"]["profile"].as_array() {
            root_profiles.extend(profiles.iter().filter_map(Value::as_str).map(str::to_owned));
        }
        if let Some(typ) = resource["resourceType"].as_str() {
            root_profiles.push(typ.to_owned());
        }
        let applied_children =
            profiles::applied_children(resource, self.resolver.as_ref(), &root_profiles);
        for profile in &root_profiles {
            if let Some(typ) = self.profile_type(profile)
                && !type_matches(&typ, resource["resourceType"].as_str().unwrap_or(""))
            {
                invalid = true;
                issues.push(issue(
                    "error",
                    "structure",
                    "リソース型と対象プロファイルの型が一致しません",
                    "resourceType",
                ));
            }
        }
        let target_findings =
            references::target_profiles(resource, self.resolver.as_ref(), &root_profiles);
        if !resource.is_object() || resource["resourceType"].as_str().is_none_or(str::is_empty) {
            invalid = true;
            issues.push(issue(
                "error",
                "structure",
                "resourceType を持つFHIRリソースを指定してください",
                "resourceType",
            ));
        } else if self
            .resolver
            .resolve(resource["resourceType"].as_str().unwrap())
            .is_none()
        {
            incomplete = true;
            issues.push(issue(
                "warning",
                "not-supported",
                "resourceType の定義が見つかりません",
                "resourceType",
            ));
        } else if self
            .resolver
            .resolve(resource["resourceType"].as_str().unwrap())
            .is_some_and(|s| s.kind.as_deref() != Some("resource"))
        {
            invalid = true;
            issues.push(issue(
                "error",
                "structure",
                "resourceType はFHIRリソース型を指定してください",
                "resourceType",
            ));
        } else {
            let opts = ValidationOptions {
                profiles: profile
                    .into_iter()
                    .chain(extra_profiles.iter().cloned())
                    .collect(),
                unknown_profile: UnknownProfilePolicy::Error,
                ..Default::default()
            };
            let result = self.validator.validate_sync(resource, &opts);
            let mut constraint_ids = HashSet::new();
            let constraints: Vec<_> = result
                .deferred
                .iter()
                .filter_map(|d| match d {
                    Deferred::Constraint {
                        path,
                        id,
                        expression,
                        severity,
                        ..
                    } if !belongs_to_child(path)
                        && severity.as_deref() != Some("guideline")
                        && constraint_ids.insert((path.clone(), id.clone())) =>
                    {
                        Some(DeferredConstraint {
                            path,
                            id,
                            expression,
                        })
                    }
                    _ => None,
                })
                .collect();
            let generic: Vec<_> = constraints
                .iter()
                .copied()
                .filter(|c| {
                    invariants::local(resource, c, &self.terminology)
                        .or_else(|| invariants::trivial(resource, c))
                        .is_none()
                        && !unsupported_constraint(c, check_references)
                        && !(c.expression == "htmlChecks()" && matches!(c.id, "txt-1" | "txt-2"))
                })
                .collect();
            let mut generic_results = self
                .evaluator
                .evaluate_all(resource, FhirVersion::R4, &generic)
                .into_iter();
            for c in &constraints {
                let trivial = invariants::local(resource, c, &self.terminology)
                    .or_else(|| invariants::trivial(resource, c));
                let unsupported = trivial.is_none() && unsupported_constraint(c, check_references);
                let evaluated = if let Some(result) = trivial {
                    result
                } else if unsupported {
                    ConstraintOutcome::NotEvaluable(
                        "unsupported context or function; not executed".into(),
                    )
                } else if c.expression == "htmlChecks()" && matches!(c.id, "txt-1" | "txt-2") {
                    narrative::evaluate(resource, c.path, c.id)
                } else {
                    generic_results.next().expect("aligned constraint results")
                };
                let severity = result
                    .deferred
                    .iter()
                    .find_map(|d| match d {
                        Deferred::Constraint {
                            path, id, severity, ..
                        } if path == c.path && id == c.id => severity.as_deref(),
                        _ => None,
                    })
                    .unwrap_or("error");
                if unsupported || matches!(evaluated, ConstraintOutcome::NotEvaluable(_)) {
                    incomplete = true;
                    deferred_checks += 1;
                    issues.push(issue(
                        "warning",
                        "not-supported",
                        &format!("FHIRPath {} を完了できません: {evaluated:?}", c.id),
                        c.path,
                    ));
                } else if evaluated == ConstraintOutcome::Failed {
                    invalid |= severity == "error";
                    issues.push(issue(
                        severity,
                        "invariant",
                        &format!("FHIRPath {}: {}", c.id, c.expression),
                        c.path,
                    ));
                }
            }
            for d in &result.deferred {
                if let Deferred::Binding {
                    path,
                    binding,
                    value,
                    ..
                } = d
                {
                    if belongs_to_child(path) {
                        continue;
                    }
                    if !matches!(binding.strength.as_deref(), Some("required" | "extensible")) {
                        continue;
                    }
                    match self.terminology.member(&binding.value_set, value) {
                        Some(true) => {}
                        Some(false) if binding.strength.as_deref() == Some("required") => {
                            invalid = true;
                            issues.push(issue(
                                "error",
                                "code-invalid",
                                &format!(
                                    "指定ValueSetに含まれないコードです: {}",
                                    binding.value_set
                                ),
                                path,
                            ));
                        }
                        _ => {
                            incomplete = true;
                            deferred_checks += 1;
                            issues.push(issue(
                                "warning",
                                "not-supported",
                                &format!("用語bindingを判定できません: {}", binding.value_set),
                                path,
                            ));
                        }
                    }
                }
            }
            for error in result.errors {
                let missing = matches!(
                    error.kind,
                    ErrorKind::UnknownSchema | ErrorKind::UnknownProfile
                );
                incomplete |= missing;
                invalid |= !missing && error.severity == Severity::Error;
                let severity = if missing || error.severity == Severity::Warning {
                    "warning"
                } else {
                    "error"
                };
                let code = if missing {
                    "not-supported"
                } else {
                    "structure"
                };
                issues.push(issue(
                    severity,
                    code,
                    &error.message,
                    &helios_fhir_validator::dotted_to_fhirpath(&error.path),
                ));
            }
        }
        let mut reference_findings = target_findings;
        if check_references {
            reference_findings.extend(references::check(resource));
        }
        for (code, path, message) in reference_findings {
            if code == "not-supported" {
                incomplete = true;
                deferred_checks += 1;
            } else {
                invalid = true;
            }
            issues.push(issue(
                if code == "not-supported" {
                    "warning"
                } else {
                    "error"
                },
                code,
                &message,
                &path,
            ));
        }
        for (path, child) in child_resources(resource) {
            let inherited = applied_children
                .get(&(child as *const Value as usize))
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let child_report = self.validate_inner(child, None, false, inherited);
            invalid |= child_report.status == "invalid";
            incomplete |= child_report.status == "incomplete" || child_report.deferred_checks > 0;
            deferred_checks += child_report.deferred_checks;
            for mut finding in child_report.outcome["issue"]
                .as_array()
                .cloned()
                .unwrap_or_default()
            {
                if finding["severity"] == "information" {
                    continue;
                }
                if let Some(expressions) =
                    finding.get_mut("expression").and_then(Value::as_array_mut)
                {
                    for expression in expressions {
                        if let Some(s) = expression.as_str() {
                            let child_type = child["resourceType"].as_str().unwrap_or("");
                            let relative = s
                                .strip_prefix(child_type)
                                .and_then(|s| s.strip_prefix('.'))
                                .unwrap_or(s);
                            *expression = if s == child_type {
                                json!(path)
                            } else {
                                json!(format!("{path}.{relative}"))
                            };
                        }
                    }
                }
                issues.push(finding);
            }
        }
        for definition in &self.definitions {
            if !used_profiles
                .iter()
                .any(|p| p.split('|').next() == Some(definition.url.as_str()))
            {
                continue;
            }
            for warning in &definition.conversion_warnings {
                if !profiles::warning_active(resource, definition.conditional_warnings.get(warning))
                {
                    continue;
                }
                incomplete = true;
                deferred_checks += 1;
                issues.push(issue(
                    "warning",
                    "not-supported",
                    &format!("{warning} ({})", definition.url),
                    resource["resourceType"].as_str().unwrap_or("Resource"),
                ));
            }
        }
        if incomplete {
            issues.push(issue(
                "warning",
                "incomplete",
                "必要な検証を完了できていません。適合済みとして送信しないでください。",
                "",
            ));
        }
        if issues.is_empty() {
            issues.push(issue(
                "information",
                "informational",
                "適用した機械検証ルールに違反はありません",
                "",
            ));
        }
        Report {
            status: if invalid {
                "invalid"
            } else if incomplete {
                "incomplete"
            } else {
                "valid"
            },
            fhir_version: "4.0.1",
            engine: ENGINE,
            structural_checks_passed: !invalid && !incomplete,
            deferred_checks,
            definitions: self.definitions.clone(),
            packages: self.packages.clone(),
            outcome: json!({ "resourceType": "OperationOutcome", "issue": issues }),
        }
    }
}

fn belongs_to_child(path: &str) -> bool {
    path.split('.')
        .any(|segment| matches!(segment, "resource" | "contained"))
}

fn type_matches(expected: &str, actual: &str) -> bool {
    expected == actual
        || expected == "Resource"
        || (expected == "DomainResource"
            && !matches!(actual, "Bundle" | "Binary" | "Parameters" | "Resource"))
}

fn unsupported_constraint(c: &DeferredConstraint<'_>, root: bool) -> bool {
    (c.path.contains('.')
        && (c.expression.contains("%resource")
            || c.expression.contains("%rootResource")
            || c.expression.contains("%context")))
        || (!root && c.expression.contains("%rootResource"))
        || c.expression
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .any(|token| matches!(token, "resolve" | "memberOf" | "conformsTo"))
}

fn child_resources(v: &Value) -> Vec<(String, &Value)> {
    let mut out = Vec::new();
    if let Some(entries) = v["entry"].as_array() {
        for (i, entry) in entries.iter().enumerate() {
            if entry.get("resource").is_some() {
                out.push((format!("Bundle.entry[{i}].resource"), &entry["resource"]));
            }
        }
    }
    if let Some(contained) = v["contained"].as_array() {
        for (i, child) in contained.iter().enumerate() {
            out.push((format!("Resource.contained[{i}]"), child));
        }
    }
    out
}

fn collect_profiles(v: &Value, out: &mut HashSet<String>) {
    if let Some(url) = v.get("url").and_then(Value::as_str) {
        out.insert(url.to_owned());
    }
    if let Some(profiles) = v
        .get("meta")
        .and_then(|m| m.get("profile"))
        .and_then(Value::as_array)
    {
        out.extend(profiles.iter().filter_map(Value::as_str).map(str::to_owned));
    }
    match v {
        Value::Object(m) => {
            for child in m.values() {
                if child.is_object() || child.is_array() {
                    collect_profiles(child, out);
                }
            }
        }
        Value::Array(a) => {
            for child in a {
                collect_profiles(child, out);
            }
        }
        _ => {}
    }
}

fn issue(severity: &str, code: &str, message: &str, path: &str) -> Value {
    let mut v = json!({ "severity": severity, "code": code, "diagnostics": message });
    if !path.is_empty() {
        v["expression"] = json!([path]);
    }
    v
}

pub mod http {
    use super::*;
    use axum::{
        Json, Router,
        extract::{
            DefaultBodyLimit, OriginalUri, Query, State,
            rejection::{JsonRejection, QueryRejection},
        },
        http::StatusCode,
        middleware::{self, Next},
        response::{IntoResponse, Response},
        routing::{get, post},
    };
    use serde::Deserialize;
    use tokio::sync::Semaphore;

    #[derive(Clone)]
    struct HttpState {
        service: Arc<Service>,
        slots: Arc<Semaphore>,
    }

    pub fn router(service: Service, concurrency: usize) -> Router {
        Router::new()
            .route("/health", get(|| async { "ok" }))
            .route("/capabilities", get(capabilities))
            .route("/metadata", get(metadata))
            .route("/validate", post(validate))
            .route("/$validate", post(validate))
            .route("/{resource_type}/$validate", post(validate))
            .fallback(|| async { failure(StatusCode::NOT_FOUND, "not-found", "unknown endpoint") })
            .layer(DefaultBodyLimit::max(MAX_INPUT_BYTES))
            .layer(middleware::from_fn(no_store))
            .with_state(HttpState {
                service: Arc::new(service),
                slots: Arc::new(Semaphore::new(concurrency.max(1))),
            })
    }

    async fn no_store(request: axum::extract::Request, next: Next) -> Response {
        let mut response = next.run(request).await;
        response
            .headers_mut()
            .insert("cache-control", "no-store".parse().unwrap());
        response
    }

    async fn capabilities(State(st): State<HttpState>) -> Json<Value> {
        Json(json!({ "fhirVersion": "4.0.1", "engine": ENGINE,
            "validation": "structure-fhirpath-terminology-references", "conformanceComplete": false,
            "definitions": st.service.definitions, "packages": st.service.packages, "maxInputBytes": MAX_INPUT_BYTES }))
    }

    async fn metadata() -> impl IntoResponse {
        (
            [("content-type", "application/fhir+json")],
            Json(json!({
                "resourceType":"CapabilityStatement", "status":"draft", "date":"2026-11-17",
                "kind":"capability", "fhirVersion":"4.0.1", "format":["application/fhir+json"],
                "software":{"name":"helios-jp-validator","version":env!("CARGO_PKG_VERSION")},
                "description":"Offline Rust validator. General validation only; unsupported rules return incomplete. See /capabilities for installed packages and current coverage.",
                "rest":[{"mode":"server","operation":[{"name":"validate","definition":"http://hl7.org/fhir/OperationDefinition/Resource-validate"}]}]
            })),
        )
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Options {
        profile: Option<String>,
    }

    async fn validate(
        State(st): State<HttpState>,
        OriginalUri(uri): OriginalUri,
        opts: Result<Query<Options>, QueryRejection>,
        body: Result<Json<Value>, JsonRejection>,
    ) -> Response {
        let opts = match opts {
            Ok(Query(opts)) => opts,
            Err(_) => {
                return failure(
                    StatusCode::BAD_REQUEST,
                    "invalid",
                    "invalid query parameters",
                );
            }
        };
        let Json(mut resource) = match body {
            Ok(v) => v,
            Err(e) => {
                return failure(
                    e.status(),
                    "invalid",
                    "FHIR JSONを受け取れませんでした（形式・Content-Type・サイズを確認してください）",
                );
            }
        };
        let mut profile = opts.profile;
        let standard_operation = uri.path().ends_with("/$validate");
        if standard_operation && resource["resourceType"] == "Parameters" {
            let Some(parameters) = resource["parameter"].as_array() else {
                return failure(
                    StatusCode::BAD_REQUEST,
                    "invalid",
                    "missing operation parameters",
                );
            };
            let mut input = None;
            for p in parameters {
                match p["name"].as_str() {
                    Some("resource") if input.is_none() => input = p.get("resource").cloned(),
                    Some("profile") if profile.is_none() => {
                        let Some(value) = p
                            .get("valueUri")
                            .or_else(|| p.get("valueCanonical"))
                            .and_then(Value::as_str)
                            .filter(|v| !v.is_empty())
                        else {
                            return failure(
                                StatusCode::BAD_REQUEST,
                                "invalid",
                                "profile must be a canonical URI",
                            );
                        };
                        profile = Some(value.to_owned());
                    }
                    Some("mode") if p["valueCode"] == "general" => {}
                    _ => {
                        return failure(
                            StatusCode::BAD_REQUEST,
                            "not-supported",
                            "unsupported or duplicate validation parameter",
                        );
                    }
                }
            }
            let Some(input) = input else {
                return failure(
                    StatusCode::BAD_REQUEST,
                    "required",
                    "resource parameter is required",
                );
            };
            resource = input;
        }
        let segments: Vec<_> = uri.path().trim_matches('/').split('/').collect();
        if segments.len() == 2
            && standard_operation
            && resource["resourceType"].as_str() != Some(segments[0])
        {
            return failure(
                StatusCode::BAD_REQUEST,
                "structure",
                "resourceType does not match operation path",
            );
        }
        let Ok(permit) = st.slots.try_acquire_owned() else {
            return failure(
                StatusCode::TOO_MANY_REQUESTS,
                "throttled",
                "validation concurrency limit reached",
            );
        };
        // The permit lives in the worker even if its HTTP client disconnects.
        match tokio::task::spawn_blocking(move || {
            let _permit = permit;
            st.service.validate(&resource, profile)
        })
        .await
        {
            Ok(report) if standard_operation => {
                let mut outcome = report.outcome;
                outcome["extension"] = json!([{"url":"https://genshiai.github.io/helios-jp/StructureDefinition/validation-status","valueCode":report.status}]);
                if report.status == "incomplete" {
                    outcome["issue"].as_array_mut().unwrap().push(issue(
                        "error",
                        "not-supported",
                        "必要な検証が未完了です",
                        "",
                    ));
                }
                ([("content-type", "application/fhir+json")], Json(outcome)).into_response()
            }
            Ok(report) => Json(report).into_response(),
            Err(_) => failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                "exception",
                "validation worker failed",
            ),
        }
    }

    fn failure(status: StatusCode, code: &str, message: &str) -> Response {
        (
            status,
            [("content-type", "application/fhir+json")],
            Json(json!({
                "resourceType": "OperationOutcome", "issue": [issue("error", code, message, "")]
            })),
        )
            .into_response()
    }
}
