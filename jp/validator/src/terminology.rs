// SPDX-License-Identifier: Apache-2.0
//! Three-valued offline terminology membership. Missing definitions never pass.
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

#[derive(Default)]
pub struct Terminology {
    sets: HashMap<String, Arc<Value>>,
    systems: HashMap<String, Arc<Value>>,
}
impl Terminology {
    pub fn insert(&mut self, v: Value) {
        let Some(url) = v["url"].as_str().map(str::to_owned) else {
            return;
        };
        let target = match v["resourceType"].as_str() {
            Some("ValueSet") => &mut self.sets,
            Some("CodeSystem") => &mut self.systems,
            _ => return,
        };
        let v = Arc::new(v);
        if let Some(version) = v["version"].as_str() {
            target.insert(format!("{url}|{version}"), v.clone());
        }
        target.insert(url, v);
    }

    pub fn member(&self, url: &str, value: &Value) -> Option<bool> {
        if let Some(code) = value.as_str() {
            return self.code(url, None, None, code, &mut HashSet::new());
        }
        if let Some(codings) = value["coding"].as_array() {
            if codings.is_empty() {
                return Some(false);
            }
            let results: Vec<_> = codings.iter().map(|v| self.coding(url, v)).collect();
            return union(&results);
        }
        if value.get("code").is_some() {
            return self.coding(url, value);
        }
        Some(false)
    }

    fn coding(&self, url: &str, v: &Value) -> Option<bool> {
        let code = v["code"].as_str()?;
        let system = v["system"].as_str();
        let version = v["version"].as_str();
        if let Some(version) = version {
            let cs = self.systems.get(&format!("{}|{version}", system?))?;
            if !contains_code(cs["concept"].as_array()?, None, code) {
                return (cs["content"] == "complete").then_some(false);
            }
        }
        self.code(url, system, version, code, &mut HashSet::new())
    }

    fn code(
        &self,
        url: &str,
        system: Option<&str>,
        version: Option<&str>,
        code: &str,
        visiting: &mut HashSet<String>,
    ) -> Option<bool> {
        if !visiting.insert(url.to_owned()) {
            return None;
        }
        let result = self.evaluate(url, system, version, code, visiting);
        visiting.remove(url);
        result
    }

    fn evaluate(
        &self,
        url: &str,
        system: Option<&str>,
        version: Option<&str>,
        code: &str,
        visiting: &mut HashSet<String>,
    ) -> Option<bool> {
        let set = self.sets.get(url)?;
        if let Some(contains) = set["expansion"]["contains"].as_array() {
            if version.is_some() {
                return None;
            }
            if contains_code(contains, system, code) {
                return Some(true);
            }
            let total = set["expansion"]["total"].as_u64()?;
            if set["expansion"]["offset"].as_u64().unwrap_or(0) != 0
                || total != count_codes(contains)
            {
                return None;
            }
            return Some(false);
        }
        let compose = set.get("compose")?;
        if compose.get("inactive").is_some() {
            return None;
        }
        let includes = compose["include"].as_array()?;
        let found = union(
            &includes
                .iter()
                .map(|v| self.component(v, system, version, code, visiting))
                .collect::<Vec<_>>(),
        );
        if found != Some(true) {
            return found;
        }
        if let Some(excludes) = compose["exclude"].as_array() {
            match union(
                &excludes
                    .iter()
                    .map(|v| self.component(v, system, version, code, visiting))
                    .collect::<Vec<_>>(),
            ) {
                Some(true) => return Some(false),
                None => return None,
                Some(false) => {}
            }
        }
        Some(true)
    }

    fn component(
        &self,
        part: &Value,
        system: Option<&str>,
        version: Option<&str>,
        code: &str,
        visiting: &mut HashSet<String>,
    ) -> Option<bool> {
        let mut checks = Vec::new();
        if let Some(sys) = part["system"].as_str() {
            if system.is_some_and(|s| s != sys) {
                return Some(false);
            }
            let selected = part["version"].as_str();
            if matches!((selected,version),(Some(a),Some(b)) if a!=b) {
                return Some(false);
            }
            let key = selected
                .or(version)
                .map_or_else(|| sys.to_owned(), |v| format!("{sys}|{v}"));
            if let Some(filters) = part["filter"].as_array() {
                let cs = self.systems.get(&key)?;
                if cs["content"] != "complete" || cs["hierarchyMeaning"] != "is-a" {
                    return None;
                }
                let concepts = cs["concept"].as_array()?;
                if has_parent_properties(concepts) {
                    return None;
                }
                if !contains_code(concepts, None, code) {
                    return Some(false);
                }
                let mut included = true;
                for filter in filters {
                    if filter["property"] != "concept" {
                        return None;
                    }
                    let ancestor = filter["value"].as_str()?;
                    let relation = descendant(concepts, ancestor, code, false);
                    included &= match filter["op"].as_str()? {
                        "is-a" => relation,
                        "descendent-of" => code != ancestor && relation,
                        "is-not-a" => !relation,
                        _ => return None,
                    };
                }
                checks.push(Some(included));
            } else if let Some(concepts) = part["concept"].as_array() {
                checks.push(Some(concepts.iter().any(|v| v["code"] == code)));
            } else {
                let cs = self.systems.get(&key)?;
                if contains_code(cs["concept"].as_array()?, None, code) {
                    checks.push(Some(true));
                } else if cs["content"] == "complete" {
                    checks.push(Some(false));
                } else {
                    checks.push(None);
                }
            }
        }
        if let Some(sets) = part["valueSet"].as_array() {
            for set in sets {
                checks.push(self.code(set.as_str()?, system, version, code, visiting));
            }
        }
        if checks.is_empty() || checks.contains(&None) && !checks.contains(&Some(false)) {
            None
        } else {
            Some(checks.iter().all(|v| *v == Some(true)))
        }
    }
}

fn has_parent_properties(concepts: &[Value]) -> bool {
    concepts.iter().any(|c| {
        c["property"].as_array().is_some_and(|p| {
            p.iter()
                .any(|p| matches!(p["code"].as_str(), Some("parent" | "subsumedBy")))
        }) || c["concept"]
            .as_array()
            .is_some_and(|c| has_parent_properties(c))
    })
}

fn descendant(concepts: &[Value], ancestor: &str, code: &str, below: bool) -> bool {
    concepts.iter().any(|concept| {
        let below = below || concept["code"] == ancestor;
        (below && concept["code"] == code)
            || concept["concept"]
                .as_array()
                .is_some_and(|children| descendant(children, ancestor, code, below))
    })
}

fn union(values: &[Option<bool>]) -> Option<bool> {
    if values.contains(&Some(true)) {
        Some(true)
    } else if values.contains(&None) {
        None
    } else {
        Some(false)
    }
}
fn contains_code(values: &[Value], system: Option<&str>, code: &str) -> bool {
    fn walk(
        values: &[Value],
        requested: Option<&str>,
        code: &str,
        inherited: Option<&str>,
    ) -> bool {
        values.iter().any(|value| {
            let actual = value["system"].as_str().or(inherited);
            (value["code"] == code && requested.is_none_or(|s| actual.is_none_or(|a| a == s)))
                || value
                    .get("contains")
                    .or_else(|| value.get("concept"))
                    .and_then(Value::as_array)
                    .is_some_and(|children| walk(children, requested, code, actual))
        })
    }
    walk(values, system, code, None)
}

fn count_codes(values: &[Value]) -> u64 {
    values
        .iter()
        .map(|v| {
            u64::from(v.get("code").is_some())
                + v["contains"].as_array().map_or(0, |v| count_codes(v))
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn nested_expansion_inherits_its_parent_code_system() {
        let mut tx = Terminology::default();
        tx.insert(json!({"resourceType":"ValueSet","url":"urn:nested","expansion":{"total":2,"contains":[{"system":"urn:correct","code":"parent","contains":[{"code":"child"}]}]}}));
        assert_eq!(
            tx.member(
                "urn:nested",
                &json!({"system":"urn:correct","code":"child"})
            ),
            Some(true)
        );
        assert_eq!(
            tx.member("urn:nested", &json!({"system":"urn:wrong","code":"child"})),
            Some(false)
        );
    }
    #[test]
    fn pinned_coding_and_compose_versions_resolve_exact_releases() {
        let mut tx = Terminology::default();
        tx.insert(json!({"resourceType":"CodeSystem","url":"urn:codes","version":"1","content":"complete","concept":[{"code":"old"}]}));
        tx.insert(json!({"resourceType":"CodeSystem","url":"urn:codes","version":"2","content":"complete","concept":[{"code":"new"}]}));
        tx.insert(json!({"resourceType":"ValueSet","url":"urn:all","compose":{"include":[{"system":"urn:codes"}]}}));
        tx.insert(json!({"resourceType":"ValueSet","url":"urn:v1","compose":{"include":[{"system":"urn:codes","version":"1"}]}}));
        tx.insert(json!({"resourceType":"ValueSet","url":"urn:import-v1","compose":{"include":[{"valueSet":["urn:v1"]}]}}));
        assert_eq!(
            tx.member(
                "urn:all",
                &json!({"system":"urn:codes","version":"1","code":"old"})
            ),
            Some(true)
        );
        assert_eq!(
            tx.member(
                "urn:all",
                &json!({"system":"urn:codes","version":"1","code":"new"})
            ),
            Some(false)
        );
        assert_eq!(
            tx.member(
                "urn:all",
                &json!({"system":"urn:codes","version":"missing","code":"old"})
            ),
            None
        );
        assert_eq!(
            tx.member("urn:v1", &json!({"system":"urn:codes","code":"old"})),
            Some(true)
        );
        assert_eq!(
            tx.member(
                "urn:import-v1",
                &json!({"system":"urn:codes","version":"2","code":"new"})
            ),
            Some(false)
        );
    }
    #[test]
    fn hierarchy_filters_include_descendants_and_exclude_abstract_parent() {
        let mut tx = Terminology::default();
        tx.insert(json!({"resourceType":"CodeSystem","url":"urn:tree","content":"complete","hierarchyMeaning":"is-a","concept":[{"code":"root","concept":[{"code":"child","concept":[{"code":"grandchild"}]}]},{"code":"other"}]}));
        tx.insert(json!({"resourceType":"ValueSet","url":"urn:descendants","compose":{"include":[{"system":"urn:tree","filter":[{"property":"concept","op":"is-a","value":"root"}]}],"exclude":[{"system":"urn:tree","concept":[{"code":"root"}]}]}}));
        assert_eq!(tx.member("urn:descendants", &json!("child")), Some(true));
        assert_eq!(
            tx.member("urn:descendants", &json!("grandchild")),
            Some(true)
        );
        assert_eq!(tx.member("urn:descendants", &json!("root")), Some(false));
        assert_eq!(tx.member("urn:descendants", &json!("other")), Some(false));
        assert_eq!(tx.member("urn:descendants", &json!("missing")), Some(false));
    }
    #[test]
    fn required_codes_imports_and_exclusions_are_checked_offline() {
        let mut tx = Terminology::default();
        tx.insert(json!({"resourceType":"CodeSystem","url":"urn:test","content":"complete","concept":[{"code":"a"},{"code":"b"}]}));
        tx.insert(json!({"resourceType":"ValueSet","url":"urn:all","version":"1","compose":{"include":[{"system":"urn:test"}],"exclude":[{"system":"urn:test","concept":[{"code":"b"}]}]}}));
        tx.insert(json!({"resourceType":"ValueSet","url":"urn:import","compose":{"include":[{"valueSet":["urn:all|1"]}]}}));
        assert_eq!(
            tx.member(
                "urn:import",
                &json!({"coding":[{"system":"urn:test","code":"a"}]})
            ),
            Some(true)
        );
        assert_eq!(tx.member("urn:import", &json!("b")), Some(false));
        assert_eq!(tx.member("urn:import", &json!("c")), Some(false));
        assert_eq!(tx.member("urn:missing", &json!("a")), None);
        assert_eq!(tx.member("urn:all|2", &json!("a")), None);
    }
    #[test]
    fn cycles_filters_partial_expansions_and_unknown_versions_do_not_pass() {
        let mut tx = Terminology::default();
        tx.insert(json!({"resourceType":"ValueSet","url":"urn:cycle","compose":{"include":[{"valueSet":["urn:cycle"]}]}}));
        tx.insert(json!({"resourceType":"ValueSet","url":"urn:filtered","compose":{"include":[{"system":"urn:test","filter":[{"property":"concept","op":"is-a","value":"a"}]}]}}));
        tx.insert(json!({"resourceType":"ValueSet","url":"urn:partial","expansion":{"total":2,"contains":[{"system":"urn:test","code":"a"}]}}));
        for url in ["urn:cycle", "urn:filtered", "urn:partial"] {
            assert_eq!(tx.member(url, &json!("missing")), None);
        }
        assert_eq!(
            tx.member(
                "urn:partial",
                &json!({"system":"urn:test","code":"a","version":"unknown"})
            ),
            None
        );
        assert_eq!(tx.member("urn:partial",&json!({"coding":[{"system":"urn:other","code":"x"},{"system":"urn:test","code":"a"}]})),Some(true));
    }
}
