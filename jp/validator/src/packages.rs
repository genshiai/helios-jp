// SPDX-License-Identifier: Apache-2.0
//! Offline, checksum-pinned FHIR package ingestion. No archive extraction or HTTP.
use anyhow::{Context, Result, ensure};
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::HashMap, io::Read, path::Path};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Package {
    pub name: String,
    pub version: String,
    pub file: String,
    pub sha256: String,
    pub source: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Lock {
    packages: Vec<Package>,
    #[serde(default)]
    fixtures: Vec<Asset>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Asset {
    file: String,
    sha256: String,
    source: String,
}

/// Operator-invoked preparation only. Validation itself never performs HTTP.
pub fn prepare(path: &Path) -> Result<()> {
    let lock: Lock = serde_json::from_reader(std::fs::File::open(path)?)?;
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(90))
        .build()?;
    let assets = lock
        .packages
        .into_iter()
        .map(|p| Asset {
            file: p.file,
            sha256: p.sha256,
            source: p.source,
        })
        .chain(lock.fixtures);
    for asset in assets {
        ensure!(
            asset.sha256.len() == 64 && hex::decode(&asset.sha256).is_ok(),
            "invalid checksum"
        );
        let target = path.parent().unwrap_or(Path::new(".")).join(asset.file);
        if target.exists() {
            ensure!(
                hex::encode(Sha256::digest(std::fs::read(&target)?)) == asset.sha256,
                "existing asset checksum mismatch: {}",
                target.display()
            );
            continue;
        }
        ensure!(
            asset.source.starts_with("https://"),
            "package sources must use HTTPS"
        );
        let mut data = Vec::new();
        let response = client.get(&asset.source).send()?.error_for_status()?;
        ensure!(
            response.url().scheme() == "https",
            "insecure download redirect"
        );
        response.take(64 * 1024 * 1024 + 1).read_to_end(&mut data)?;
        ensure!(data.len() <= 64 * 1024 * 1024, "asset too large");
        ensure!(
            hex::encode(Sha256::digest(&data)) == asset.sha256,
            "download checksum mismatch"
        );
        std::fs::create_dir_all(target.parent().context("missing parent")?)?;
        use std::io::Write;
        // create_new protects pre-existing files, even when another preparer races us.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)?;
        file.write_all(&data)?;
    }
    Ok(())
}

pub fn load(path: &Path) -> Result<(Vec<Package>, Vec<Value>)> {
    let lock: Lock = serde_json::from_reader(std::fs::File::open(path)?)?;
    let mut available = HashMap::from([("hl7.fhir.r4.core".to_owned(), "4.0.1".to_owned())]);
    let mut names = std::collections::HashSet::new();
    for p in &lock.packages {
        ensure!(names.insert(&p.name), "duplicate package: {}", p.name);
        ensure!(
            !available.contains_key(&p.name) || p.name == "hl7.fhir.r4.core",
            "duplicate package: {}",
            p.name
        );
        available.insert(p.name.clone(), p.version.clone());
    }
    let mut resources = Vec::new();
    for p in &lock.packages {
        let bytes = std::fs::read(path.parent().unwrap_or(Path::new(".")).join(&p.file))?;
        ensure!(bytes.len() <= 64 * 1024 * 1024, "package too large");
        ensure!(
            hex::encode(Sha256::digest(&bytes)) == p.sha256,
            "checksum mismatch: {}",
            p.name
        );
        let mut archive =
            tar::Archive::new(GzDecoder::new(bytes.as_slice()).take(512 * 1024 * 1024));
        let mut manifest = None;
        let mut count = 0;
        for entry in archive.entries()? {
            let mut entry = entry?;
            count += 1;
            ensure!(count <= 20000, "too many archive entries");
            let name = entry.path()?.into_owned();
            ensure!(
                !name.is_absolute()
                    && !name
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir)),
                "unsafe archive path"
            );
            if !entry.header().entry_type().is_file() {
                continue;
            }
            if name.extension().and_then(|v| v.to_str()) != Some("json") {
                continue;
            }
            ensure!(entry.size() <= 64 * 1024 * 1024, "archive entry too large");
            let mut data = Vec::new();
            entry.read_to_end(&mut data)?;
            let mut resource: Value = serde_json::from_slice(&data)
                .with_context(|| format!("invalid JSON in {}", p.name))?;
            if name.ends_with("package.json") {
                manifest = Some(resource);
            } else if resource.get("resourceType").is_some() {
                if resource["resourceType"] == "StructureDefinition"
                    && resource.get("version").is_none()
                {
                    resource["version"] = Value::String(p.version.clone());
                }
                resources.push(resource);
            }
        }
        let manifest = manifest.context("missing package.json")?;
        ensure!(
            manifest["name"] == p.name && manifest["version"] == p.version,
            "package identity mismatch: {}",
            p.name
        );
        let versions = manifest
            .get("fhirVersions")
            .or_else(|| manifest.get("fhir-version-list"));
        ensure!(
            versions
                .and_then(Value::as_array)
                .is_some_and(|v| v.iter().all(|v| v == "4.0.1") && !v.is_empty()),
            "package must target R4 4.0.1"
        );
        if let Some(dependencies) = manifest["dependencies"].as_object() {
            for (name, version) in dependencies {
                ensure!(
                    available.get(name).map(String::as_str) == version.as_str(),
                    "missing or incompatible dependency: {name}"
                );
            }
        }
    }
    Ok((lock.packages, resources))
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use serde_json::json;

    fn fixture(dependencies: Value) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("package.tgz");
        let encoder = GzEncoder::new(
            std::fs::File::create(&file).unwrap(),
            Compression::default(),
        );
        let mut archive = tar::Builder::new(encoder);
        let data = serde_json::to_vec(&json!({"name":"test-package","version":"1.0.0","fhirVersions":["4.0.1"],"dependencies":dependencies})).unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive
            .append_data(&mut header, "package/package.json", data.as_slice())
            .unwrap();
        archive.into_inner().unwrap().finish().unwrap();
        let lock = dir.path().join("lock.json");
        std::fs::write(&lock,serde_json::to_vec(&json!({"packages":[{"name":"test-package","version":"1.0.0","file":"package.tgz","source":"https://example.org/package.tgz","sha256":hex::encode(Sha256::digest(std::fs::read(file).unwrap()))}]})).unwrap()).unwrap();
        (dir, lock)
    }

    #[test]
    fn checks_archive_digest_identity_and_dependencies() {
        let (dir, lock) = fixture(json!({"hl7.fhir.r4.core":"4.0.1"}));
        assert!(load(&lock).is_ok());
        let original = std::fs::read_to_string(&lock).unwrap();
        std::fs::write(&lock, original.replace("test-package", "wrong-package")).unwrap();
        assert!(
            load(&lock)
                .unwrap_err()
                .to_string()
                .contains("identity mismatch")
        );
        std::fs::write(&lock, original).unwrap();
        std::fs::write(dir.path().join("package.tgz"), b"tampered").unwrap();
        assert!(
            load(&lock)
                .unwrap_err()
                .to_string()
                .contains("checksum mismatch")
        );
        let (_dir, lock) = fixture(json!({"missing-dependency":"1.0.0"}));
        assert!(
            load(&lock)
                .unwrap_err()
                .to_string()
                .contains("missing or incompatible dependency")
        );
    }
}
