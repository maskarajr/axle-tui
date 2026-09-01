# ⚙️ Axle

**Axle** is a lightweight, terminal-driven Windows Driver Manager built with Rust and Ratatui. It interfaces directly with native Windows Update Agent (WUA) COM APIs to provide seamless, bloat-free inspection, staging, and installation of hardware driver updates.

### ✨ Features
- **Atomic Discovery:** Accurately separates actionable updates from reboot-staged drivers.
- **Minimal & Fast:** Clean terminal UI with zero telemetry or background background bloat.
- **Safe Staging:** Real-time progress monitoring with fail-safe error and reboot tracking.
- **Native Integration:** Built-in UAC elevation and native COM session interop.