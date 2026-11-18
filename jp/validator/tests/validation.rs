// SPDX-License-Identifier: Apache-2.0
use axum::{body::Body, http::Request};
use helios_jp_validator::{Service, http::router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use tower::ServiceExt;

fn service() -> Service {
    Service::load(&[] as &[&Path]).unwrap()
}

#[test]
fn clean_resource_completes_all_available_rules() {
    let report = service().validate(&json!({"resourceType":"Patient","active":true}), None);
    assert_eq!(report.status, "valid");
    assert!(report.structural_checks_passed);
    assert!(
        report.outcome["issue"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["severity"] != "error")
    );
}

#[test]
fn catches_real_core_shape_errors() {
    for resource in [
        json!({"resourceType":"Patient","invented":true}),
        json!({"resourceType":"Patient","active":"yes"}),
        json!({"resourceType":"Patient","birthDate":"2026-02-30"}),
        json!({"resourceType":"Patient","birthDate":"あいうえお"}),
        json!({"resourceType":"Patient","name":{"family":"Test"}}),
        json!({"resourceType":"Observation"}),
        json!({"resourceType":"Observation","status":"final","code":{"text":"Test"},"valueString":"a","valueBoolean":true}),
        json!({}),
        json!([]),
        json!({"resourceType":2}),
        json!({"resourceType":"HumanName"}),
    ] {
        let report = service().validate(&resource, None);
        assert_eq!(report.status, "invalid", "{resource}: {}", report.outcome);
        assert!(!report.structural_checks_passed);
    }
}

#[test]
fn unknown_profile_and_resource_are_incomplete_not_valid_or_rule_violations() {
    for (resource, profile) in [
        (
            json!({"resourceType":"Patient"}),
            Some("https://example.org/missing".into()),
        ),
        (json!({"resourceType":"NotAResource"}), None),
        (
            json!({"resourceType":"Patient","meta":{"profile":["https://example.org/missing"]}}),
            None,
        ),
    ] {
        let report = service().validate(&resource, profile);
        assert_eq!(report.status, "incomplete");
        assert!(!report.structural_checks_passed);
    }
}

struct Fixture(PathBuf);
impl Fixture {
    fn new(value: Value) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "helios-jp-{}-{unique}-{sequence}.json",
            std::process::id()
        ));
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn definition() -> Value {
    json!({"resourceType":"StructureDefinition","url":"https://example.org/NamedPatient","name":"NamedPatient","version":"1.0.0","fhirVersion":"4.0.1","kind":"resource","type":"Patient","derivation":"constraint","baseDefinition":"http://hl7.org/fhir/StructureDefinition/Patient","differential":{"element":[
        {"id":"Patient","path":"Patient"},
        {"id":"Patient.name","path":"Patient.name","min":1,"max":"*","type":[{"code":"HumanName"}]}
    ]}})
}

#[test]
fn differential_without_type_inherits_choice_and_still_rejects_multiple_branches() {
    let sd = Fixture::new(
        json!({"resourceType":"StructureDefinition","url":"https://example.org/InheritedPatient","name":"InheritedPatient","version":"1","fhirVersion":"4.0.1","kind":"resource","type":"Patient","derivation":"constraint","baseDefinition":"http://hl7.org/fhir/StructureDefinition/Patient","differential":{"element":[
            {"id":"Patient","path":"Patient"},{"id":"Patient.deceased[x]","path":"Patient.deceased[x]"}
        ]}}),
    );
    let service = Service::load(&[&sd.0]).unwrap();
    let profile = Some("https://example.org/InheritedPatient".into());
    let resource = json!({"resourceType":"Patient","deceasedBoolean":false});
    assert_eq!(service.validate(&resource, profile.clone()).status, "valid");
    let resource =
        json!({"resourceType":"Patient","deceasedBoolean":false,"deceasedDateTime":"2026-01-01"});
    assert_eq!(service.validate(&resource, profile).status, "invalid");
}

#[test]
fn distinct_resource_profile_slices_validate_profile_content_not_profile_labels() {
    let patient = Fixture::new(definition());
    let bundle = Fixture::new(
        json!({"resourceType":"StructureDefinition","url":"https://example.org/PatientBundle","name":"PatientBundle","version":"1","fhirVersion":"4.0.1","kind":"resource","type":"Bundle","derivation":"constraint","baseDefinition":"http://hl7.org/fhir/StructureDefinition/Bundle","differential":{"element":[
            {"id":"Bundle","path":"Bundle"},
            {"id":"Bundle.entry","path":"Bundle.entry","slicing":{"discriminator":[{"type":"profile","path":"resource"}],"rules":"closed"}},
            {"id":"Bundle.entry:patient","path":"Bundle.entry","sliceName":"patient","min":1,"max":"1"},
            {"id":"Bundle.entry:patient.resource","path":"Bundle.entry.resource","min":1,"type":[{"code":"Patient","profile":["https://example.org/NamedPatient"]}]}
        ]}}),
    );
    let service = Service::load(&[&patient.0, &bundle.0]).unwrap();
    let profile = Some("https://example.org/PatientBundle".into());
    let mut resource = json!({"resourceType":"Bundle","type":"collection","entry":[{"fullUrl":"urn:uuid:12345678-1234-4321-9123-123456789012","resource":{"resourceType":"Patient","name":[{"family":"Test"}]}}]});
    let report = service.validate(&resource, profile.clone());
    // The local structural fixtures deliberately do not install terminology.
    assert_eq!(report.status, "incomplete", "{}", report.outcome);
    resource["entry"][0]["resource"]
        .as_object_mut()
        .unwrap()
        .remove("name");
    assert_eq!(
        service.validate(&resource, profile.clone()).status,
        "invalid"
    );
    resource["entry"] = json!([]);
    assert_eq!(service.validate(&resource, profile).status, "invalid");
}

#[test]
fn applies_local_profile_and_preserves_version_identity() {
    let fixture = Fixture::new(definition());
    let service = Service::load(&[&fixture.0]).unwrap();
    let profile = "https://example.org/NamedPatient|1.0.0";
    assert_eq!(
        service
            .validate(&json!({"resourceType":"Patient"}), Some(profile.into()))
            .status,
        "invalid"
    );
    assert!(
        service
            .validate(
                &json!({"resourceType":"Patient","name":[{"family":"Test"}]}),
                Some(profile.into())
            )
            .structural_checks_passed
    );
    let wrong_version = service.validate(
        &json!({"resourceType":"Patient"}),
        Some("https://example.org/NamedPatient|2.0.0".into()),
    );
    assert_eq!(wrong_version.status, "incomplete");
    assert!(!wrong_version.structural_checks_passed);
}

#[test]
fn refuses_duplicate_canonicals_and_wrong_fhir_version() {
    let fixture = Fixture::new(definition());
    assert!(Service::load(&[&fixture.0, &fixture.0]).is_err());
    let mut sd = definition();
    sd["fhirVersion"] = json!("5.0.0");
    let fixture = Fixture::new(sd);
    assert!(Service::load(&[&fixture.0]).is_err());
}

#[test]
fn fhirpath_catches_cross_field_invariants_and_unavailable_terminology_stays_incomplete() {
    let report = service().validate(
        &json!({"resourceType":"Patient","contact":[{"gender":"unknown"}]}),
        None,
    );
    assert_eq!(report.status, "invalid");
    assert!(report.outcome["issue"].as_array().unwrap().iter().any(|i| {
        i["diagnostics"]
            .as_str()
            .is_some_and(|s| s.contains("pat-1"))
    }));
    let report = service().validate(&json!({"resourceType":"Patient","gender":"invalid"}), None);
    assert_eq!(report.status, "incomplete");
    assert!(report.deferred_checks > 0);
}

#[test]
fn detects_dangling_contained_references_and_duplicate_bundle_urls() {
    let report = service().validate(&json!({"resourceType":"Observation","status":"final","code":{"text":"test"},"subject":{"reference":"#missing"}}),None);
    assert_eq!(report.status, "invalid");
    assert!(
        report.outcome["issue"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["code"] == "not-found")
    );
    let report=service().validate(&json!({"resourceType":"Bundle","type":"collection","entry":[
        {"fullUrl":"urn:uuid:00000000-0000-0000-0000-000000000001","resource":{"resourceType":"Patient"}},
        {"fullUrl":"urn:uuid:00000000-0000-0000-0000-000000000001","resource":{"resourceType":"Patient"}}
    ]}),None);
    assert_eq!(report.status, "invalid");
    assert!(
        report.outcome["issue"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["code"] == "duplicate")
    );
}

#[tokio::test]
async fn http_reports_findings_and_does_not_cache_patient_data() {
    let response = router(service(), 1)
        .oneshot(
            Request::post("/validate")
                .header("content-type", "application/fhir+json")
                .body(Body::from(r#"{"resourceType":"Patient","active":"yes"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let value: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(value["status"], "invalid");
    assert_eq!(value["outcome"]["resourceType"], "OperationOutcome");
}

#[tokio::test]
async fn http_rejects_malformed_and_oversized_payloads_with_outcome() {
    for (body, status) in [
        ("{".to_owned(), 400),
        (" ".repeat(helios_jp_validator::MAX_INPUT_BYTES + 1), 413),
    ] {
        let response = router(service(), 1)
            .oneshot(
                Request::post("/validate")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        let value: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(value["resourceType"], "OperationOutcome");
    }
}

#[tokio::test]
async fn standard_validate_accepts_parameters_and_marks_incomplete_as_error() {
    for (payload, expected) in [
        (
            json!({"resourceType":"Parameters","parameter":[{"name":"resource","resource":{"resourceType":"Patient"}}]}),
            "valid",
        ),
        (
            json!({"resourceType":"Patient","meta":{"profile":["https://example.org/not-installed"]}}),
            "incomplete",
        ),
    ] {
        let response = router(service(), 1)
            .oneshot(
                Request::post("/Patient/$validate")
                    .header("content-type", "application/fhir+json")
                    .body(Body::from(payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-type"], "application/fhir+json");
        let outcome: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(outcome["resourceType"], "OperationOutcome");
        assert_eq!(outcome["extension"][0]["valueCode"], expected);
        if expected == "incomplete" {
            assert!(
                outcome["issue"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|i| i["severity"] == "error")
            );
        }
    }
}

#[tokio::test]
async fn standard_validate_rejects_wrong_type_and_unsupported_modes() {
    for (path, body) in [
        ("/Observation/$validate", json!({"resourceType":"Patient"})),
        ("/$validate?mode=create", json!({"resourceType":"Patient"})),
        (
            "/$validate",
            json!({"resourceType":"Parameters","parameter":[{"name":"resource","resource":{"resourceType":"Patient"}},{"name":"mode","valueCode":"delete"}]}),
        ),
    ] {
        let response = router(service(), 1)
            .oneshot(
                Request::post(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
    }
}

#[test]
fn cli_uses_distinct_failure_and_incomplete_exit_codes() {
    for (resource, expected) in [
        (json!({"resourceType":"Patient","gender":"unknown"}), 3),
        (json!({"resourceType":"Patient","active":42}), 1),
    ] {
        let fixture = Fixture::new(resource);
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_helios-jp-validator"))
            .arg("validate")
            .arg(&fixture.0)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(expected));
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_ne!(report["status"], "valid");
    }
}

#[test]
fn inherited_extension_slice_keeps_its_url_and_required_value() {
    let extension = Fixture::new(
        json!({"resourceType":"StructureDefinition","url":"https://example.org/RequiredText","name":"RequiredText","version":"1","fhirVersion":"4.0.1","kind":"complex-type","type":"Extension","derivation":"constraint","baseDefinition":"http://hl7.org/fhir/StructureDefinition/Extension","differential":{"element":[{"id":"Extension","path":"Extension"},{"id":"Extension.url","path":"Extension.url","fixedUri":"https://example.org/RequiredText"},{"id":"Extension.value[x]","path":"Extension.value[x]","min":1,"type":[{"code":"string"}]}]}}),
    );
    let base = Fixture::new(
        json!({"resourceType":"StructureDefinition","url":"https://example.org/BaseExtPatient","name":"BaseExtPatient","version":"1","fhirVersion":"4.0.1","kind":"resource","type":"Patient","derivation":"constraint","baseDefinition":"http://hl7.org/fhir/StructureDefinition/Patient","differential":{"element":[{"id":"Patient","path":"Patient"},{"id":"Patient.extension","path":"Patient.extension","slicing":{"discriminator":[{"type":"value","path":"url"}],"rules":"open"}},{"id":"Patient.extension:text","path":"Patient.extension","sliceName":"text","min":0,"max":"1","type":[{"code":"Extension","profile":["https://example.org/RequiredText"]}]}]}}),
    );
    let child = Fixture::new(
        json!({"resourceType":"StructureDefinition","url":"https://example.org/ChildExtPatient","name":"ChildExtPatient","version":"1","fhirVersion":"4.0.1","kind":"resource","type":"Patient","derivation":"constraint","baseDefinition":"https://example.org/BaseExtPatient","differential":{"element":[{"id":"Patient","path":"Patient"},{"id":"Patient.extension:text","path":"Patient.extension","sliceName":"text","min":1,"max":"1"}]}}),
    );
    let service = Service::load(&[&extension.0, &base.0, &child.0]).unwrap();
    let profile = Some("https://example.org/ChildExtPatient".into());
    let good = json!({"resourceType":"Patient","extension":[{"url":"https://example.org/RequiredText","valueString":"synthetic"}]});
    assert_eq!(service.validate(&good, profile.clone()).status, "valid");
    assert_eq!(
        service
            .validate(&json!({"resourceType":"Patient"}), profile.clone())
            .status,
        "invalid",
        "{:?}",
        service.compiled_schema("https://example.org/ChildExtPatient")
    );
    assert_eq!(
        service
            .validate(
                &json!({"resourceType":"Patient","extension":[]}),
                profile.clone()
            )
            .status,
        "invalid"
    );
    let bad = json!({"resourceType":"Patient","extension":[{"url":"https://example.org/RequiredText","valueBoolean":true}]});
    assert_eq!(service.validate(&bad, profile).status, "invalid");
}
