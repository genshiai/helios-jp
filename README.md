# Helios JP

Experimental, unofficial JP Core / JP-CLINS support built on [Helios HFS](https://github.com/HeliosSoftware/hfs).

**This is not a complete CLINS conformance implementation or a national-service certification.** It is an alpha release of a native Rust CLI and HTTP validator. Unknown or unsupported required checks produce `incomplete`, never `valid`.

This fork preserves Helios history and its MIT license. Our standalone JP validator lives in [`jp/validator`](jp/validator), under **Apache-2.0**. It currently pins the published Helios libraries at **0.2.7**; the upstream source snapshot in this fork is 0.2.9. The JP executable does not build or use the upstream HFS server. It needs neither Java/JVM nor PostgreSQL.

## Start

From this repository root:

```sh
cargo build --manifest-path jp/validator/Cargo.toml --locked
cargo run --manifest-path jp/validator/Cargo.toml --locked -- --packages jp/validator/clins-packages.lock.json prepare
cargo run --manifest-path jp/validator/Cargo.toml --locked -- --packages jp/validator/clins-packages.lock.json serve
```

The listener defaults to `127.0.0.1:8091`. It is an unauthenticated local service. `prepare` downloads public checksum-pinned definitions. Validation itself makes no external terminology or reference requests and does not log resource bodies.

```sh
curl -H 'Content-Type: application/fhir+json' \
  --data-binary @jp/validator/tests/fixtures/patient.json \
  http://127.0.0.1:8091/validate
```

The HTTP API is also usable from TypeScript, Python or any HTTP client. [`jp/validator/README.md`](jp/validator/README.md) documents commands, endpoints, tests and remaining limitations.

## Licenses and status

- Upstream Helios: [MIT](LICENSE.md), unchanged.
- Original JP validator additions: [Apache-2.0](jp/validator/LICENSE), with [NOTICE](jp/validator/NOTICE).
- FHIR packages, terminology and official examples retain their provider licenses. They are downloaded separately rather than bundled into this release.
- The new executable has no electronic medical record dependency, credentials, patient database or private repository history. Checked-in JP fixtures are synthetic.
- This project is not affiliated with, approved by, or certified by Helios Software, HL7, the Japanese government or the CLINS publishers.

Upstream documentation is preserved in [README.upstream.md](README.upstream.md). Upstream workflows are archived in `.github/upstream-workflows`; only the JP workflows run on this fork. No crates.io publication or hosted public validation endpoint is part of this release.
