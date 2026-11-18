// SPDX-License-Identifier: Apache-2.0
//! Definition-only inventory, not evidence that every rule was exercised.
//! Run after prepare; output contains no clinical data.
use helios_jp_validator::Service;
use serde_json::{Value, json};
use std::{io::Read, path::Path};

fn main() -> anyhow::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let lock: Value =
        serde_json::from_reader(std::fs::File::open(root.join("clins-packages.lock.json"))?)?;
    // Check the pinned digests and identities before examining any definition.
    let service = Service::from_lock(&root.join("clins-packages.lock.json"))?;
    let mut profiles = Vec::new();
    for package in lock["packages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["name"] == "jp-eCSCLINS.r4" || p["name"] == "jp-core.r4")
    {
        let file = std::fs::File::open(root.join(package["file"].as_str().unwrap()))?;
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
        for entry in archive.entries()? {
            let mut entry = entry?;
            if !entry.path()?.to_string_lossy().ends_with(".json") {
                continue;
            }
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            let sd: Value = serde_json::from_slice(&bytes)?;
            if sd["resourceType"] != "StructureDefinition" {
                continue;
            }
            let mut rules = Vec::new();
            for element in sd["differential"]["element"]
                .as_array()
                .into_iter()
                .flatten()
            {
                for constraint in element["constraint"].as_array().into_iter().flatten() {
                    rules.push(json!({"kind":"invariant","element":element["id"],"key":constraint["key"],"severity":constraint["severity"],"expression":constraint["expression"]}));
                }
                if element.get("binding").is_some() {
                    rules.push(json!({"kind":"binding","element":element["id"],"binding":element["binding"]}));
                }
                if element.get("slicing").is_some() {
                    rules.push(json!({"kind":"slicing","element":element["id"],"slicing":element["slicing"]}));
                }
            }
            let warnings = service
                .definitions
                .iter()
                .find(|d| d.url == sd["url"])
                .map(|d| d.conversion_warnings.clone())
                .unwrap_or_default();
            profiles.push(json!({"package":package["name"],"canonical":sd["url"],"version":sd["version"],"base":sd["baseDefinition"],"conversion_warnings":warnings,"rules":rules}));
        }
    }
    profiles.sort_by_key(|p| p["canonical"].as_str().unwrap_or("").to_owned());
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"schema_version":1,"exhaustive_conformance_proven":false,"scope":"JP Core and CLINS differential invariants, bindings and slicing; inherited R4 rules and element constraints also apply","packages":service.packages,"profiles":profiles})
        )?
    );
    Ok(())
}
