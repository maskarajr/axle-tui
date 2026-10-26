# Changelog

All notable changes to axle-tui are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Version numbers follow the rules in `AGENTS.md`.

## [Unreleased]

## [0.2.0] - 2026-10-26

### Added

- Driver scan and install go through Windows Update COM.
- Downloads and installs use separate queues. Up to 3 downloads run at once, smallest first. One installer runs after a package has downloaded. A base package installs before an extension. A successful install that needs a reboot stops the rest.

### Changed

- The package is published as `axle-tui` with crates.io categories set.

### Fixed

- An aborted install is no longer shown as a clean completion while rows stay pending.
- After a reboot is required, the session finishes without waiting for downloads that are already running.

## [0.1.0] - 2026-09-01

### Added

- First terminal release for installing pending Windows drivers through Windows Update.
- GitHub Actions builds a Windows zip when a version tag is pushed.

### Security

- Terminal text from driver titles is sanitized.

[Unreleased]: https://github.com/maskarajr/axle-tui/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/maskarajr/axle-tui/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/maskarajr/axle-tui/releases/tag/v0.1.0
