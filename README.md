# ⚙️ Axle

**Axle** is a lightweight, terminal-driven Windows Driver Manager built with Rust and Ratatui. It interfaces directly with native Windows Update Agent (WUA) COM APIs to provide seamless, bloat-free inspection, staging, and installation of hardware driver updates.

### ✨ Features
- **Atomic Discovery:** Accurately separates actionable updates from reboot-staged drivers.
- **Minimal & Fast:** Clean terminal UI with zero telemetry or background bloat.
- **Safe Staging:** Up to three downloads run at once, smallest first. One installer runs after a download succeeds. An extension waits behind its base driver. A reboot stops the rest.
- **Native Integration:** Built-in UAC elevation and native COM session interop.

### 🔒 Security & Privileges
- **Administrator required:** The release binary embeds a `requireAdministrator` manifest. Windows prompts for UAC on launch. Driver download and installation need that elevation.
- **Trusted catalog only:** Updates are discovered and installed exclusively through the Windows Update Agent — Axle does not download from arbitrary URLs.
- **GUID-based install:** Selected drivers are installed by WUA update ID, not title alone. A selected update is dropped when another selected update lists that id as superseded.
- **Run from source you trust:** Build failures for the Windows resource manifest (icon/UAC) abort compilation rather than silently shipping a misconfigured binary.

### Usage
```powershell
# List pending drivers (headless)
axle-tui --list

# Interactive TUI (default)
axle-tui
```
