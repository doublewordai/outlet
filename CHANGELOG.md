# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.0](https://github.com/doublewordai/outlet/compare/v0.4.5...v0.5.0) - 2026-02-02

### Other

- Revert "Revert "allow trace_ids to be picked up for joint up tracing ([#38](https://github.com/doublewordai/outlet/pull/38))" (…" ([#43](https://github.com/doublewordai/outlet/pull/43))

## [0.4.5](https://github.com/doublewordai/outlet/compare/v0.4.4...v0.4.5) - 2026-01-26

### Other

- Merge pull request #35 from doublewordai/renovate/flamegraph-0.x-lockfile
- Merge pull request #34 from doublewordai/renovate/thiserror-2.x-lockfile
- Merge pull request #36 from doublewordai/renovate/uuid-1.x-lockfile
- *(deps)* update rust crate uuid to v1.20.0
- *(deps)* update rust crate criterion to 0.8
- Merge pull request #24 from doublewordai/renovate/axum-monorepo
- Merge pull request #25 from doublewordai/renovate/tokio-test-0.x-lockfile
- Merge pull request #27 from doublewordai/release-plz-2026-01-07T07-52-39Z

## [0.4.4](https://github.com/doublewordai/outlet/compare/v0.4.3...v0.4.4) - 2026-01-07

### Added

- use JoinSet for concurrent handler execution

### Other

- cargo fmt

## [0.4.3](https://github.com/doublewordai/outlet/compare/v0.4.2...v0.4.3) - 2026-01-07

### Added

- add queue depth metrics

### Other

- Merge pull request #19 from doublewordai/renovate/uuid-1.x-lockfile
- Merge pull request #20 from doublewordai/renovate/bytes-1.x-lockfile
- Merge pull request #14 from doublewordai/renovate/serde_json-1.x-lockfile
- Merge pull request #18 from doublewordai/renovate/tokio-tracing-monorepo
- Merge pull request #15 from doublewordai/renovate/thiserror-2.x-lockfile
- Merge pull request #17 from doublewordai/renovate/tower-http-0.x-lockfile
- Merge pull request #16 from doublewordai/renovate/tokio-util-0.x-lockfile
- Merge pull request #22 from doublewordai/renovate/hyper-1.x-lockfile
- *(deps)* update rust crate hyper to v1.8.1
- *(deps)* update rust crate flamegraph to v0.6.10
- Merge pull request #10 from doublewordai/renovate/anyhow-1.x-lockfile
- *(deps)* update rust crate axum to v0.8.7
- Add renovate.json
- docs

## [0.4.2](https://github.com/doublewordai/outlet/compare/v0.4.1...v0.4.2) - 2025-11-11

### Fixed

- quieten some logs

## [0.4.1](https://github.com/doublewordai/outlet/compare/v0.4.0...v0.4.1) - 2025-11-07

### Added

- request filtering in outlet

## [0.4.0](https://github.com/doublewordai/outlet/compare/v0.3.0...v0.4.0) - 2025-09-25

### Fixed

- install rustfmt and clippy components in CI workflow
- account for time-to-first-byte vs. stream ending

## [0.3.0](https://github.com/doublewordai/outlet/compare/v0.2.0...v0.3.0) - 2025-09-02

### Added

- pass full RequestData to handle_response and enhance correlation ID generation

## [0.2.0](https://github.com/doublewordai/outlet/compare/v0.1.0...v0.2.0) - 2025-08-27

### Added

- (breaking) store headers as bytes so that axum::HeaderMap isnt in the external interface
- (breaking) make trait async

## [0.1.0] - 2025-08-27

### Added
- Initial release of outlet crate
- HTTP request/response logging middleware for Axum
- Request body capture and streaming support
- Configurable size limits and capture settings
- Correlation ID tracking