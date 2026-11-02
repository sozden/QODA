# Changelog

All notable changes to QODA are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- Volume control for the broadcast's audio (0–400%) and an optional automatic level that keeps
  the loudness about the same from broadcast to broadcast; both apply at once and are remembered.
  The audio is now decoded by ffmpeg and scaled by QODA before ffplay plays it.
- LNB LO: a menu of common LO frequencies (including 0 for no LNB) and a button that sets the LO
  to 0 and, pressed again, returns to the previous LO.
- Full screen with the keyboard: F11, or F while no text field is being edited.

### Fixed
- Watching a broadcast picked in the wideband view lost frames every second or so with a network
  receiver (LibreSDR): at 12 MS/s the link drops samples. Watch now first narrows to the lowest
  sample rate that holds the broadcast (at least twice its bandwidth), centred on it.

### Changed
- The yAtölye badge next to the QODA title is gone; the name stays in the status bar.

## [0.2.0] - 2026-11-01

### Added
- DVB-S2 transmitter, first step: `qoda-tx` modulates an MPEG-TS file into a SigMF IQ recording
  (QPSK, 8PSK and 16APSK, normal and short frames, with or without pilots), at an integer number
  of samples per symbol suitable for the ADALM-Pluto. Nothing is transmitted yet; the output is
  checked with QODA's own receiver, which a loopback test does automatically.
- Transmitting with an ADALM-Pluto (experimental): `qoda-tx --pluto <uri> --freq <downlink MHz>`
  sends the signal on the QO-100 wideband uplink. It refuses frequencies whose signal would leave
  the 2401.5–2409.5 MHz uplink band, starts at 30 dB attenuation, limits the time on air and
  switches the transmitter off on Ctrl+C. `--pattern CALLSIGN` makes a test broadcast with
  FFmpeg at the channel's exact bit rate. `qoda-tx` is included in the Windows and Linux packages.
  First air test on 2026-11-01: TA4SO's test broadcast at 125 kS/s QPSK 1/2 went through QO-100
  and was decoded by QODA (SNR 4.1 dB).
- `qoda-tx` moves the signal away from the Pluto's LO (`--shift`), so the LO leakage no longer sits
  in the middle of the signal; `--level` sets the digital drive; `--loopback` checks the link to
  the Pluto without transmitting, and the achieved sample rate is shown while sending.

- QODA TX (`qoda-tx-gui`), a window for the transmitter in the same look as QODA: downlink
  frequency tuner with uplink, Pluto LO, bandwidth and TS capacity; source (test broadcast with
  the call sign, or a TS file, also by drag and drop); symbol rate, MODCOD, frame and roll-off;
  level, attenuation, time limit and frequency correction; a TRANSMIT / STOP button, a link test
  through the digital loopback, an ON AIR banner with time and achieved sample rate, and a log.
  English and Turkish; settings are kept in `tx-settings.txt` next to QODA's settings. The
  transmit logic moved to `qoda::transmit`, shared with `qoda-tx`.
- QODA TX asks before the first transmission of a session for a confirmation (licensed, signal
  kept below the beacon) next to a short note on starting with high attenuation; TRANSMIT stays
  disabled until it is ticked. The link test needs no confirmation.
- A TS file's mux rate is measured from its PCRs and compared with the channel's capacity: QODA TX
  shows it and refuses a file more than 3% off, with the `-muxrate` to use; `qoda-tx` refuses it
  too unless `--ignore-rate` is given.
- README: "QODA TX: your first transmission" step by step, with screenshots, in English and Turkish.
- The transmitter plays its TS as one endless stream, modulated as it is sent: on every pass
  the continuity counters carry on and PCR/PTS/DTS move on by the length of the file, so the
  20 s test broadcast loops without the continuity errors and the picture hiccup at each restart.
  The test broadcast is muxed at the channel's rate to the bit. Transmitting starts sooner, as
  the signal is no longer modulated in advance. `qoda-tx --out … --loops N` writes N passes as
  sent on air; `qoda-analyze --stream` reports continuity errors.
- Automatic frequency correction while watching: when the decoder measures the carrier more than
  1.5% of the symbol rate (at least 1 kHz) off the selected frequency, QODA moves the selection
  onto it and restarts the decoder.

### Fixed
- Weak signals at low symbol rates were decoded with a wrong MODCOD (e.g. QPSK 2/5 instead of 1/2)
  and lost every frame when the tuning was a few kHz off (since 0.1.2). The pilot-based frequency
  estimate was taken as absolute although it is only unambiguous within ±π/24 rad/symbol (±2.6 kHz
  at 125 kS/s); it now corrects the current estimate, and single outlying measurements are limited.
  Found with an IQ recording of the first QODA-to-QODA transmission through QO-100.
- With the 3 dB detection limit, the transponder noise seen through the receiver's own passband
  (a plateau ~0.77 of the sample rate wide in the middle of the spectrum) showed up as a
  ~1800 kS/s "signal" in the signals strip. Such a plateau is now ignored.
- `qoda-analyze`: with both `--freq` and `--rs` given, the frequency is used as the exact centre.
- The symbol rate of narrow signals was measured too low (66 kS/s as 58 kS/s): the spectrum is
  now smoothed over linear power instead of dB.

## [0.1.4] - 2026-10-31

### Changed
- "Locked" now means frames are decoding. When frame headers are found but no frame decodes,
  QODA shows "Signal too weak" in red (watch card, video area and spectrum band) instead of a
  green "Locked" next to an empty picture.

### Fixed
- The symbol rate of a signal only a few dB above the noise floor was measured too high (e.g.
  662 instead of 500 kS/s): the half-power width is now taken with the noise power subtracted.
- Returning from the wideband view lost the broadcast if it had been picked in the wideband view
  and fell outside the previous band; QODA now centres on it. The switch also restarts the decoder
  once instead of twice.

## [0.1.3] - 2026-10-31

### Fixed
- Signals less than 6 dB above the noise floor were not detected, so with a small dish the
  QO-100 beacon (about 4 dB above the floor on 60 cm) was not found and its symbol rate not set.
  The limit is now 3 dB.

## [0.1.2] - 2026-10-31

### Added
- Linux release files: a `.deb` package for Ubuntu 24.04+ / Debian 13 (with menu entry and udev
  rules for USB receivers) and a `.tar.gz` for other distributions, built by `tools/linux/package.sh`
  and attached to tagged releases by CI. Also attached to the 0.1.1 release.

- Full screen video: a button on the picture or a double click; Esc leaves.
- The wideband view button returns to the previous sample rate and frequency when pressed again.
- The selected signal's band on the spectrum turns green and reads "LOCKED" while the decoder is
  locked on it.
- The uplink (TX) frequency for the selected signal, downlink − 8089.5 MHz, for information.

### Changed
- Better reception of weak signals, measured with a new sensitivity test against the thresholds
  of EN 302 307-1:
  - LDPC decoding uses belief propagation, with min-sum as a fallback. Min-sum alone lost up to
    2.5 dB on the low code rates.
  - Soft bits are scaled from an SNR estimate that stays correct on weak signals; the old one
    made weak signals look stronger. 8PSK now reaches the standard's thresholds, and QPSK 3/5 and
    above come within 0.5 dB of them.
  - On weak signals with pilots, the carrier phase is taken from the pilot blocks instead of a
    loop that slips when its decisions go wrong.
  - Fewer frames are lost while the receiver locks on a weak signal.

### Fixed
- Under WSL the program closed when the window was resized (WSLg's Wayland compositor drops the
  connection); QODA now uses X11 there.

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

[Unreleased]: https://github.com/sozden/QODA/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/sozden/QODA/compare/v0.1.4...v0.2.0
[0.1.4]: https://github.com/sozden/QODA/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/sozden/QODA/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/sozden/QODA/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/sozden/QODA/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/sozden/QODA/releases/tag/v0.1.0
