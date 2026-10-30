# Changelog

All notable changes to QODA are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- Linux release files: a `.deb` package for Ubuntu 24.04+ / Debian 13 (with menu entry and udev
  rules for USB receivers) and a `.tar.gz` for other distributions, built by `tools/linux/package.sh`
  and attached to tagged releases by CI. Also attached to the 0.1.1 release.

## [0.1.1] - 2026-10-30

### Added
- Linux support: build instructions in the README and a CI job that builds and tests on Ubuntu 24.04.
  Tested live with a LibreSDR and an RTL-SDR (through WSL).
- macOS build instructions and a CI build on Apple Silicon (not yet tested with hardware).
- Screenshot in the README.

### Changed
- Code comments and test messages are in English.
- The "FFmpeg not found" hint suggests the system package manager on Linux and macOS.

### Fixed
- When a receiver rejects a frequency, gain or sample rate change while streaming, QODA reopens it
  with the new settings instead of stopping. This happens with an RTL-SDR passed to WSL through USB/IP.
- The frame counters showed empty boxes instead of ✓/✗, which the UI font cannot draw; they now read
  "Frames OK" and "Frames lost".

## [0.1.0] - 2026-10-30

First release.

### Added
- DVB-S2 receiver written from scratch from ETSI EN 302 307-1: Gardner timing recovery, PL header
  sync and PLS decoding, PL descrambling, decision-directed carrier tracking, soft demapping,
  layered min-sum LDPC decoding (all normal and short frame codes), BCH check, BBFRAME
  descrambling and MPEG-TS extraction.
- QPSK, 8PSK and 16APSK, with or without pilots, normal and short frames. 8PSK and 16APSK are
  verified with synthetic signals only.
- Live spectrum and waterfall; click a signal to select it, double-click to centre on it, drag
  to move the receiver, mouse wheel to step the selection.
- Automatic detection of centre frequency, symbol rate and roll-off from the spectrum, and a strip
  listing every signal in view.
- LNB LO and RF entry with the IF computed automatically; digit-by-digit frequency tuning.
- Embedded video (through FFmpeg) and audio (through ffplay), with service name, provider and
  stream types from the DVB tables.
- Receivers: RTL-SDR and HackRF through SoapySDR, ADALM-Pluto and LibreSDR through libiio
  (USB or network).
- Wideband view of the whole QO-100 wideband transponder at up to 12 MS/s.
- SigMF IQ recording and `qoda-analyze`, a command-line tool that scans and decodes recordings.
- English and Turkish user interface.
- Windows package with SoapySDR, the RTL-SDR and HackRF drivers and libiio, in two variants:
  with and without FFmpeg. `qoda --diagnose` reports what the program finds.

[Unreleased]: https://github.com/sozden/QODA/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/sozden/QODA/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/sozden/QODA/releases/tag/v0.1.0
