# Helios JP validator — alpha

Native Rust FHIR R4 / JP Core / JP-CLINS validation, implemented as a CLI, library and local HTTP service. Original additions are Apache-2.0; Helios dependencies remain MIT. No Java, PostgreSQL or electronic medical record application is required.

## Scope

Pinned definitions: R4 4.0.1, JP Core 1.1.2-clins, JP-CLINS 1.13.0, JP FHIR Terminology 2.2610.0. Sources and SHA-256 digests are in `clins-packages.lock.json`. Official specifications: <https://jpfhir.jp/fhir/clins/igv1/>.

The tool adds package verification, local terminology membership, versioned Coding and compose/import resolution, complete nested CodeSystem hierarchy filters, XHTML checks, local Bundle/contained reference and targetProfile checks, profile-converter corrections, inherited extension cardinality, calendar dates, and the pinned CLINS LTS/PTS infection-sharing expression.

It checks synthetic Patient, Condition, AllergyIntolerance, laboratory Observation, referral, discharge summary, patient care summary and legacy R4 medication collections. These examples do not prove exhaustive conformance. The medication collection is a generic R4 export, not a CLINS five-information Bundle.

## Commands

Run from the repository root:

```sh
cargo run --manifest-path jp/validator/Cargo.toml --locked -- --packages jp/validator/clins-packages.lock.json prepare
cargo run --manifest-path jp/validator/Cargo.toml --locked -- --packages jp/validator/clins-packages.lock.json validate jp/validator/tests/fixtures/patient.json
cargo run --manifest-path jp/validator/Cargo.toml --locked -- --packages jp/validator/clins-packages.lock.json serve
```

CLI exit codes: 0 valid, 1 invalid, 2 execution error, 3 incomplete. You may omit the file to read JSON from stdin. `--definition PATH` loads explicit local R4 StructureDefinitions instead of packages. Definitions and patient-supplied URLs are never fetched at validation time.

## HTTP

- `POST /validate`: direct FHIR JSON input, returns a detailed report.
- `POST /$validate`, `POST /Patient/$validate`, etc.: direct resource or FHIR Parameters input, returns OperationOutcome. General validation only.
- `GET /capabilities`: loaded packages, warnings and limits.
- `GET /metadata`: CapabilityStatement.
- `GET /health`: health check.

Default bind: numeric loopback, port 8091. No authentication is provided: do not expose this alpha endpoint publicly. Input limit 2 MiB, default concurrency 2. Responses are `no-store`. Type/profile/input violations, unsupported validation modes and overload are explicit. A 200 HTTP status means the request was processed; inspect the validation status.

## Remaining work

**`conformanceComplete` remains false.** Five definition-conversion warnings remain in the pinned set: referral Observation parent/reslicing (2), discharge nested Bundle profile slicing (2), and JP Core radiology category discrimination (1). Rules are deferred when their unsupported conditions are exercised; do not remove that guard to obtain a successful report.

Unloaded terminology or versions, versionless expansions queried with versioned Coding, complex unordered array patterns, external references, and general `resolve()`, `memberOf()` or `conformsTo()` expressions can remain incomplete. Only the exact pinned CLINS infection expression is handled specially. Clinical truth, textual/structured meaning, all profile combinations, national transport and certification are outside this alpha's evidence.

The rule inventory covers 131 JP Core/CLINS definitions, with 55 differential invariants, 395 bindings and 93 slicing declarations. Inherited R4 rules and structural element constraints also apply. Counts are an inventory, not a test coverage percentage.

## Tests

```sh
cargo fmt --manifest-path jp/validator/Cargo.toml --check
cargo test --manifest-path jp/validator/Cargo.toml --locked
# After prepare:
cargo test --manifest-path jp/validator/Cargo.toml --locked -- --ignored
cargo clippy --manifest-path jp/validator/Cargo.toml --locked --all-targets -- -D warnings
cargo run --manifest-path jp/validator/Cargo.toml --locked --example rule_inventory
```

All checked-in JP fixtures are synthetic. Official examples are downloaded verbatim with digest checks; some deliberately violate the specification. Third-party assets retain their original licenses. Do not use production patient data in issue reports, CI logs or public demo requests.

Declared MSRV is 1.90; the release is tested on the CI stable toolchain. MSRV 1.90 itself has not been verified. The isolated workspace keeps the Helios-pinned Serde version separate from the upstream root workspace.
