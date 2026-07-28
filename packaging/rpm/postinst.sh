#!/bin/sh
# RPM post-install (Fedora / RHEL / openSUSE): refresh the desktop database so the
# .desktop entry (Categories=…;TerminalEmulator;) is discoverable by launchers and
# xdg-terminal-exec (DESIGN.md §12.3).
#
# Unlike Debian there is no /usr/bin/x-terminal-emulator alternatives system on
# rpm distros, so — deliberately unlike packaging/deb/postinst.sh — we do NOT run
# update-alternatives here. Terminal discovery is via the desktop entry.
set -e

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database -q /usr/share/applications || true
fi

exit 0
