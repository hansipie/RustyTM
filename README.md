# RustyTM

[![License: MIT](https://img.shields.io/github/license/hansipie/RustyTM?color=blue)](LICENSE)
[![Language: Rust](https://img.shields.io/badge/language-Rust-dea584?logo=rust&logoColor=white)](https://www.rust-lang.org)
[![Rust edition 2021](https://img.shields.io/badge/edition-2021-orange?logo=rust&logoColor=white)](Cargo.toml)
[![Version](https://img.shields.io/github/v/release/hansipie/RustyTM?display_name=tag&sort=semver)](https://github.com/hansipie/RustyTM/releases)
[![Platform: Linux](https://img.shields.io/badge/platform-Linux-informational?logo=linux&logoColor=white)](#)
[![TUI: ratatui](https://img.shields.io/badge/TUI-ratatui-5c6bc0)](https://ratatui.rs)
[![Last commit](https://img.shields.io/github/last-commit/hansipie/RustyTM)](https://github.com/hansipie/RustyTM/commits/main)
[![Code size](https://img.shields.io/github/languages/code-size/hansipie/RustyTM)](https://github.com/hansipie/RustyTM)
[![Stars](https://img.shields.io/github/stars/hansipie/RustyTM?style=flat)](https://github.com/hansipie/RustyTM/stargazers)

<p align="center">
  <img src="docs/slideshow.svg" alt="RustyTM slideshow: Summary, Processes, Performance, System Info, Power &amp; Freq, Drivers, Disk Space, Benchmarks" width="900">
  <br>
  <sub>Slideshow of the main pages (rendered with <code>rustytm --dump=&lt;page&gt;</code>)</sub>
</p>

Task manager for Linux, written in Rust (terminal UI, ratatui). Original implementation: every
collector reads `/proc`, `/sys`, `systemctl`, `loginctl`, `nvidia-smi` directly. No licence key, no
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
