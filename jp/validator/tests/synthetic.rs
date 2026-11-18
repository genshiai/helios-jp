// SPDX-License-Identifier: Apache-2.0
use helios_jp_validator::Service;
use serde_json::{Value, json};
use std::path::Path;
fn service() -> &'static Service {
    static SERVICE: std::sync::OnceLock<Service> = std::sync::OnceLock::new();
    SERVICE.get_or_init(|| {
        Service::from_lock(&Path::new(env!("CARGO_MANIFEST_DIR")).join("clins-packages.lock.json"))
            .unwrap()
    })
}
#[test]
#[ignore = "requires pinned packages"]
fn standalone_synthetic_examples_and_missing_input_mutations() {
    let service = service();
    for name in [
        "patient",
        "conditions",
        "allergies",
        "laboratory",
        "referral",
        "discharge",
        "patient_summary",
        "medications",
    ] {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/{name}.json"));
        let resource: Value = serde_json::from_reader(std::fs::File::open(path).unwrap()).unwrap();
        let report = service.validate(&resource, None);
        assert_eq!(report.status, "valid", "{name}: {}", report.outcome);
        let mut invalid = resource;
        match name {
            "patient" => invalid["identifier"][1]["value"] = json!("invalid"),
            "conditions" => {
                invalid["entry"][1]["resource"]
                    .as_object_mut()
                    .unwrap()
                    .remove("onsetDateTime");
            }
            "allergies" | "laboratory" => {
                invalid["entry"][1]["resource"]
                    .as_object_mut()
                    .unwrap()
                    .remove("code");
            }
            "medications" => invalid["entry"][1]["resource"]["status"] = json!("invalid-status"),
            _ => {
                invalid["entry"][0]["resource"]
                    .as_object_mut()
                    .unwrap()
                    .remove("section");
            }
        }
        assert_eq!(
            service.validate(&invalid, None).status,
            "invalid",
            "{name} missing input"
        );
    }
}
#[test]
#[ignore = "requires pinned packages"]
fn infection_sharing_requires_membership_for_lts_and_pts() {
    let service = service();
    let mut resource: Value =
        serde_json::from_str(include_str!("fixtures/laboratory.json")).unwrap();
    resource["entry"][1]["resource"]
        .as_object_mut()
        .unwrap()
        .remove("valueQuantity");
    resource["entry"][1]["resource"]["valueString"] = json!("陰性");
    for indication in ["LTS", "PTS"] {
        resource["entry"][1]["resource"]["meta"]["tag"] = json!([{"system":"http://jpfhir.jp/fhir/clins/CodeSystem/JP_ehrshrs_indication","code":indication}]);
        assert_eq!(service.validate(&resource, None).status, "invalid");
        resource["entry"][1]["resource"]["code"]["coding"][0] = json!({"system":"http://jpfhir.jp/fhir/clins/CodeSystem/JLAC10/JP_CLINS_ObsLabResult_InfectionLabo_CS","code":"5E074000002306211","display":"梅毒STS(定性)"});
        let report = service.validate(&resource, None);
        assert_eq!(report.status, "valid", "{}", report.outcome);
        resource["entry"][1]["resource"]["code"]["coding"][0] = json!({"system":"http://jpfhir.jp/fhir/clins/CodeSystem/JLAC10/JP_CLINS_ObsLabResult_CoreLabo_CS","code":"3A015000001827101","display":"ALB"});
    }
}
