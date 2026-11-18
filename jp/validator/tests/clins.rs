// SPDX-License-Identifier: Apache-2.0
//! Official examples are downloaded verbatim with pinned checksums by `prepare`.
use helios_jp_validator::Service;
use serde_json::{Value, json};
use std::path::Path;

#[test]
#[ignore = "requires `--packages clins-packages.lock.json prepare`"]
fn official_clins_patient_examples_and_mutations() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let service = Service::from_lock(&root.join("clins-packages.lock.json")).unwrap();
    // "minimum" declares JP Core, not the CLINS Patient profile; do not impose
    // CLINS-only cardinalities on that separate example.
    for name in ["standard", "standard-public"] {
        let path = root.join(format!(
            "../data/fhir/Patient-Example-JP-Patient-{name}.json"
        ));
        let resource: Value = serde_json::from_reader(std::fs::File::open(path).unwrap()).unwrap();
        let report = service.validate(&resource, None);
        assert_eq!(
            report.status, "valid",
            "positive {name}: {}",
            report.outcome
        );
        let mut wrong_gender = resource.clone();
        wrong_gender["gender"] = json!("invalid");
        let report = service.validate(&wrong_gender, None);
        assert_eq!(
            report.status, "invalid",
            "wrong gender {name}: {}",
            report.outcome
        );
        let mut missing = resource.clone();
        missing.as_object_mut().unwrap().remove("gender");
        assert_eq!(service.validate(&missing, None).status, "invalid");
        let mut wrong_identifier = resource.clone();
        for identifier in wrong_identifier["identifier"].as_array_mut().unwrap() {
            if identifier["system"]
                .as_str()
                .is_some_and(|s| s.contains("memberID"))
            {
                identifier["value"] = json!("invalid");
            }
        }
        let report = service.validate(&wrong_identifier, None);
        assert_eq!(
            report.status, "invalid",
            "wrong insurance identifier {name}: {}",
            report.outcome
        );
    }
    let resource: Value = serde_json::from_reader(
        std::fs::File::open(
            root.join("../data/fhir/Patient-Example-JP-Patient-standard-public-error1Public.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let report = service.validate(&resource, None);
    assert_eq!(
        report.status, "invalid",
        "official negative: {}",
        report.outcome
    );
}

#[test]
#[ignore = "requires pinned packages"]
fn pinned_r4_core_canonical_versions_resolve_without_accepting_other_versions() {
    let service =
        Service::from_lock(&Path::new(env!("CARGO_MANIFEST_DIR")).join("clins-packages.lock.json"))
            .unwrap();
    let good = json!({"resourceType":"Patient","meta":{"profile":["http://hl7.org/fhir/StructureDefinition/Patient|4.0.1"]}});
    let report = service.validate(&good, None);
    assert_eq!(report.status, "valid", "{}", report.outcome);
    let bad = json!({"resourceType":"Patient","meta":{"profile":["http://hl7.org/fhir/StructureDefinition/Patient|5.0.0"]}});
    assert_eq!(service.validate(&bad, None).status, "incomplete");
}
