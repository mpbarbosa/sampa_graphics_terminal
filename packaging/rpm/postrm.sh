#!/bin/sh
# RPM post-uninstall: refresh the desktop database after our .desktop entry is
# removed so stale launcher entries disappear. Runs on removal and upgrade; the
# refresh is harmless in both cases.
set -e

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database -q /usr/share/applications || true
fi

exit 0
