# RustyTM

Task manager for Linux, written in Rust (terminal UI, ratatui). Original implementation: every
collector reads `/proc`, `/sys`, `systemctl`, `loginctl`, `nvidia-smi` directly. No licence, no
feature gating: all pages are enabled.

    cargo build --release
    ./target/release/rustytm                 # interactive
    ./target/release/rustytm --page=drivers
    ./target/release/rustytm --dump=connections --size=160x40   # render a page as text
    ./target/release/rustytm --self-test
    ./target/release/rustytm --record=30     # headless Flight Recorder -> *.rustytmtrace
    ./target/release/rustytm --bench=cpu|disk|gpu|internet|all

Pages: Summary, Processes (tree, signals, nice, affinity, details), Performance (CPU/memory/disks/
network/GPU), App history, Startup apps, Users, Services, System Info (export), Power & Freq,
Flight Recorder (record, save, validate, replay), Connections, Installed Apps, Drivers (kernel
modules), Disk Space (scanner), Benchmarks (CPU, disk, GPU, internet), Settings. Press `?` for keys.

The GPU benchmark (FP32 compute and memory copy bandwidth) uses OpenCL, loaded at run time with
`dlopen`: nothing to link at build time, and without an OpenCL driver (vendor driver or Mesa
rusticl) the test just reports that no GPU was found.

Not implemented: video (WebM) recording, tray gadgets / pop-out / screensaver /
global shortcut (these need a GUI toolkit), localisation, update check.
Settings: `~/.config/rustytm/settings.json`; data: `~/.local/share/rustytm/`.
