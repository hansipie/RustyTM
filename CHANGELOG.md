# Changelog

## 1.2.0 - 2026-10-28

### Added
- Setting to show network rates in bits per second (Kb/s, Mb/s, Gb/s) instead of bytes per second.
- Wi-Fi section in System Info: driver, SSID, BSSID, frequency, channel, security, signal, bit rates, power save.
- Memory details in System Info: memory modules (needs root), block size, huge pages, swap devices, swappiness, overcommit and more.
- Disk Health page (`--page=smart`): SMART verdict, temperature, power-on hours, wear and error counters through `smartctl`.
- Uninstall from Installed Apps with `u` (flatpak, snap, dnf, rpm-ostree, apt, rpm), with confirmation.

## 1.1.0

### Added
- `rustytm update` and crates.io install.
- OpenCL GPU compute and memory benchmark.

### Fixed
- Help overlay sized to its content.

## 1.0.0

- Initial release.
