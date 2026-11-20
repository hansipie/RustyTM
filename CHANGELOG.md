# Changelog

## 1.7.0 - 2026-11-20

### Added
- MCP server for AI assistants (Claude Code and other MCP clients), off by default. Turn it on from the Settings page: RustyTM then serves MCP over HTTP on `127.0.0.1` (port 7419 by default, configurable), protected by an access token generated on first use. The Settings page shows the address and copies the `claude mcp add` command. 25 read tools cover every page (overview, CPU, memory, disks, network, GPUs and their processes, history, processes and process details, sensors, batteries, connections, drivers, installed apps, disk health, startup apps, services, users, system info, app history, disk scan, benchmarks, Flight Recorder and traces, settings).
- MCP actions behind a separate setting, "MCP server: allow actions" (off by default): signals (terminate, kill, suspend, resume, process tree), priority, affinity, process report, run a program, service start/stop/restart/enable/disable, startup apps, lock or sign off sessions, uninstall, disk scan, benchmarks, Flight Recorder, system info export, clearing histories, settings and page changes. Each action is shown in the status bar.
- `--mcp-http[=PORT]` (MCP over HTTP without the UI) and `--mcp` (MCP over stdin/stdout, for clients that start RustyTM themselves).
- MCP server security: it listens on `127.0.0.1` only, refuses requests without the access token or coming from a web page of another site, and limits request size, open connections and slow clients. "New access token" on the Settings page revokes the clients set up so far. `get_trace` only reads traces from the Flight Recorder folder.

### Changed
- `settings.json` is now written readable by you only (mode 600), since it holds the MCP access token. An existing file gets these permissions on the next save.
- systemctl, loginctl and pkexec calls are stopped after 120 seconds without an answer, together with any program they started. A command already running with administrator rights cannot be stopped: RustyTM then says so and lets it finish in the background.
- Ctrl-C or SIGTERM sent to RustyTM (for example in `--mcp-http` or `--mcp` mode) also reaches the systemctl or loginctl command running at that moment.

### Fixed
- Services and Users pages: start, stop, restart, enable, disable, lock and sign off no longer freeze the screen while systemctl, loginctl or the pkexec password dialog runs. The status bar shows the action at once and its result when it ends, then the page reloads.
- A second action on a service or session that is still being changed is refused instead of running twice at the same time.
- Results of actions that end close together are all shown in the status bar, failures first; the latest result always stays visible.
- Users page: `l` on a user with several sessions locks them all as one action, and the result names the sessions that locked and each one that failed.
- "Run new task" (`N`) no longer leaves a zombie process behind once the program exits.

## 1.6.0 - 2026-11-19

### Added
- Wi-Fi network name (SSID) shown in the Performance page's network details, under the interface name. It is refreshed every 10 seconds and hidden when the interface is not connected to a Wi-Fi network.

### Fixed
- Flight Recorder replay: PgUp, PgDn, Home and End now move through the replayed samples (PgUp/PgDn by 30 samples) instead of moving the selection in the trace list.
- Filter prompt (`/`): Esc now cancels the edit and restores the previous filter. Before, the text typed so far was kept. Enter still keeps the new filter.

## 1.5.2 - 2026-11-13

### Changed
- Page order in the left list: Summary, Processes, Performance, Connections, Thermals, Battery, Power & Freq, Disk Space, Disk Health, Installed Apps, App history, Startup apps, Services, Drivers, Users, System Info, Flight Recorder, Benchmarks, Settings. The list in `rustytm --help` follows the same order. The last page you left is remembered by its position, so after updating RustyTM may reopen once on a neighbouring page.

## 1.5.1 - 2026-11-04

### Changed
- Lower CPU use and fewer allocations while RustyTM runs:
  - The screen is redrawn only after a key press, a resize, a new sample or progress of background work, instead of five times a second.
  - Tables build only the rows that fit on screen.
  - Per-process GPU data is collected only while the Processes or Performance page is open. Between full scans (every 5 passes), only known GPU clients and recently started processes are inspected.
  - The process list keeps each process's command line, owner and user name between samples instead of reading them every time. It also no longer copies the whole list every sample, and is sorted once per frame.
  - CPU frequencies are read once per sample, and `/proc/cpuinfo` only when sysfs lacks some of them. Flight Recorder samples are built only while recording.
- Release build: one codegen unit and stripped symbols (smaller binary).

### Fixed
- The history of an unplugged disk, removed network interface or vanished sensor is freed after 120 samples without readings, instead of being kept until RustyTM quits.

## 1.5.0 - 2026-11-04

### Added
- Per-process GPU usage on the GPU page and in the process list. AMD and Intel are read from the DRM client statistics in `/proc/<pid>/fdinfo`; NVIDIA from `nvidia-smi`. For each process: dedicated memory, shared memory (GTT or system) and the load of each engine, grouped as 3D, Compute, Copy and Video. Processes of other users cannot be inspected and are counted in the title of the table.
- GPU and VRAM columns in the process list, sortable like the other columns (`<` / `>`).
- Runtime detected for each GPU process from its loaded libraries (CUDA, HIP, ggml, ONNX, OpenCL, Level Zero, Vulkan, OpenGL, Video). This shows which libraries a process loaded, not what it computes.
- Update notice: RustyTM checks crates.io in the background and, when you quit, prints a message on the standard output if a newer version exists (run `rustytm update` to install it). Set `RUSTYTM_NO_UPDATE_CHECK` to turn the check off.
- GPU page: a VRAM bar split in coloured portions by runtime, a shared memory (GTT) bar, "By runtime" and "By engine" tables, CPU-visible VRAM, memory controller activity, and for NVIDIA the performance state, PCIe link and clock throttle reasons.

### Changed
- GPU page layout: on terminals of 130 columns or more, the tables are on the left and the charts, bars and details in a fixed-width column on the right, with no empty areas. Narrower terminals stack the sections and drop columns to fit.
- Combined GPU view ("All GPUs") now adds the cards up: memory, power and limits are summed, the hottest card sets the temperature, the utilization graph has one line per GPU and the memory graph covers all cards. Processes of every GPU are listed with their GPU number.
- Process list columns are narrower so that the new GPU and VRAM columns fit in a 110-column terminal without cutting values.
- Performance page: `g` and `K` only act on the CPU, and the key hints show only the keys that work for the selected device.

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
