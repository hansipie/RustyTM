# Changelog

## 1.4.0 - 2026-10-30

### Added
- Battery page (`--page=battery`) for laptops: status, charge, energy in Wh, power draw, voltage, time remaining or time to full, health against the design capacity and charge cycles, read from `/sys/class/power_supply` (peripheral batteries such as wireless mice are listed too, with their level when they report one). A charge graph shows the last hour (sampled every 10 s while RustyTM runs) followed by a forecast to empty or full at the current power, and a second graph shows the power drawn or fed. `↑↓` switch between batteries; a device without power readings shows the charge graph only. Pages after "Thermals" shift by one.

## 1.3.0 - 2026-10-28

### Added
- Thermals page (`--page=thermals`): a dashboard with one temperature graph per sensor (hwmon, ACPI thermal zones, GPU), with current, min, max and average. `↑↓`/`n`/`p` cycle through the sensors, `v` switches between the dashboard and a single-sensor view, `f` or `Enter` shows the selected graph full screen (`←→` cycle there too, `Esc` leaves).
- Disk throughput graph on the Performance page (read and write), also for all disks combined.

### Changed
- All graphs, bars and progress gauges share one look, drawn with `tui-chart`: smoothed braille area charts with a green → amber → red fill by level (CPU, memory, disk active time, GPU, temperatures), one color per direction for throughput (disk read/write, network receive/send) auto-scaled to round values in the chosen unit (bytes or bits per second), time and value axes when the graph is large enough, and min / max / average over the window in the bottom border. Bars are drawn with sub-cell precision. The Summary network graph shows receive and send together.
- Updated ratatui to 0.30.

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
