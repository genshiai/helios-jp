// SPDX-License-Identifier: Apache-2.0
//! 固定した公式架空例の受入状況を出力する。完全適合の試験とは区別する。
use helios_jp_validator::Service;
use serde_json::{Value, json};
use std::path::Path;

fn main() -> anyhow::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let lock: Value =
        serde_json::from_reader(std::fs::File::open(root.join("clins-packages.lock.json"))?)?;
    let service = Service::from_lock(&root.join("clins-packages.lock.json"))?;
    if let Some(profile) = std::env::args().nth(1) {
        println!(
            "{}",
            serde_json::to_string_pretty(&service.compiled_schema(&profile))?
        );
        return Ok(());
    }
    let mut results = Vec::new();
    for fixture in lock["fixtures"].as_array().unwrap() {
        let file = fixture["file"].as_str().unwrap();
        let resource = serde_json::from_reader(std::fs::File::open(root.join(file))?)?;
        let report = service.validate(&resource, None);
        results.push(json!({"file":file,"status":report.status,"deferred_checks":report.deferred_checks,"outcome":report.outcome}));
    }
    println!("{}", serde_json::to_string_pretty(&results)?);
    Ok(())
}
