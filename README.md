# ⚙️ Axle

**Axle** is a lightweight, terminal-driven Windows Driver Manager built with Rust and Ratatui. It interfaces directly with native Windows Update Agent (WUA) COM APIs to provide seamless, bloat-free inspection, staging, and installation of hardware driver updates.

### ✨ Features
- **Atomic Discovery:** Accurately separates actionable updates from reboot-staged drivers.
- **Minimal & Fast:** Clean terminal UI with zero telemetry or background bloat.
- **Safe Staging:** Real-time progress monitoring with fail-safe error and reboot tracking.
- **Native Integration:** Built-in UAC elevation and native COM session interop.

### 🔒 Security & Privileges
- **Administrator required:** Axle embeds a `requireAdministrator` manifest. Windows will prompt for UAC elevation on launch. This is required for WUA driver download and installation.
- **Trusted catalog only:** Updates are discovered and installed exclusively through the Windows Update Agent — Axle does not download from arbitrary URLs.
- **GUID-based install:** Selected drivers are installed by WUA update ID, not title alone, to avoid ambiguous package matching.
- **Run from source you trust:** Build failures for the Windows resource manifest (icon/UAC) abort compilation rather than silently shipping a misconfigured binary.

### Usage
```powershell
# List pending drivers (headless)
axle-tui --list

# Interactive TUI (default)
axle-tui
```
