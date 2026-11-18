# JP alpha releases

The JP executable is a separate workspace. Do not run upstream `cargo release` for a JP release: that command publishes all upstream crates and creates unrelated artifacts.

1. Run JP CI and review the exact diff and included fixtures.
2. Confirm `jp/validator` has no private app paths, credentials, third-party relicensing or bundled official FHIR archives.
3. Tag only after JP CI succeeds: `jp-v0.1.0-alpha.1`.
4. Publish a GitHub **prerelease**, explicitly noting incomplete CLINS coverage. The initial prerelease is source-only; no binary, container, hosted endpoint or crates.io publication is implied.
5. Include the upstream MIT license, JP Apache-2.0 license and NOTICE in any future binary distribution. Provider licenses for external definition packages remain separate.

Only the JP workflow is active on this fork. Upstream workflows remain versioned under `.github/upstream-workflows` as reference.
