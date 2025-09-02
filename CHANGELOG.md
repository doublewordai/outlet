# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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