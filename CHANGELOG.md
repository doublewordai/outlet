# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.3](https://github.com/doublewordai/outlet/compare/v0.4.2...v0.4.3) - 2026-01-07

### Added

- add queue depth metrics

### Other

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