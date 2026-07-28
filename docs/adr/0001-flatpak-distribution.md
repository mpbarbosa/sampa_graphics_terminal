# ADR 0001 — Flatpak distribution: declined for v1

- **Status:** Accepted (2026-07-28)
- **Context:** DESIGN.md §16 / ROADMAP M3 "[Decision] Flatpak vs. run-the-host-shell"; the
  v1 packaging bar (ROADMAP M5 Phase 5.3, v1 release checklist).

## Decision

Ship Sampa for v1 as **native packages** — `.deb`, `.rpm`, and AppImage — and **do
not** provide a Flatpak. Revisit post-v1 if there is demand.

## Why

Sampa's defining behavior is running the user's **real host shell** (their zsh, with
their `$PATH`, dotfiles, tools, and working directory) on a controlling PTY. That is
fundamentally at odds with Flatpak's sandbox model:

1. **Host shell needs a sandbox escape.** A Flatpak app cannot `exec` the host's zsh
   directly; it would have to go through `flatpak-spawn --host` (which requires the
   `org.freedesktop.Flatpak` D-Bus talk permission). That deliberately punches through
   the sandbox, negating the isolation that is Flatpak's main value — a terminal that
   can run anything on the host is not meaningfully contained.
2. **PTY / job-control semantics get harder.** Our Layer 1 relies on `portable-pty`
   giving the child a real controlling terminal so signals and job control work
   (DESIGN.md §5.1). Interposing `flatpak-spawn --host` between Sampa and the shell
   adds a process boundary that complicates preserving those semantics.
3. **Terminal registration doesn't cross the sandbox.** The whole point of the
   `x-terminal-emulator` (Debian) / `TerminalEmulator` + `xdg-terminal-exec` integration
   is that *other host apps* launch Sampa to run *host* commands. A sandboxed app can't
   serve as the system terminal for host processes.

A correct Flatpak is *possible* — terminals like Ptyxis and Black Box do it via
`flatpak-spawn --host` — but it is a separate body of work (host-spawn plumbing,
permission UX, re-testing the app matrix through the host boundary) with real caveats,
and it is not on the v1 critical path.

## Consequences

- v1 targets: `.deb` (registers `x-terminal-emulator` via `update-alternatives`), `.rpm`
  (desktop-entry discovery; **no** Debian alternatives — see `packaging/rpm/`), and the
  portable AppImage. Bundle config lives in `src-tauri/tauri.conf.json`
  (`bundle.linux.{deb,rpm}`).
- Fedora/RHEL/openSUSE users install the `.rpm`; terminal discovery is via the
  `Categories=…;TerminalEmulator;` desktop entry and `xdg-terminal-exec`.
- If Flatpak is revisited: adopt the `flatpak-spawn --host` approach, add the manifest
  under `packaging/flatpak/`, and re-run the real-app matrix through the host boundary
  before claiming support.
