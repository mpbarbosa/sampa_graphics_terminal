#!/bin/sh
# Debian post-install: register Sampa as a terminal alternative so launchers and
# `x-terminal-emulator -e` can find it (DESIGN.md §12.3), and refresh the desktop
# database so the .desktop entry shows up.
set -e

case "$1" in
  configure|abort-upgrade|abort-remove|abort-deconfigure)
    if command -v update-alternatives >/dev/null 2>&1; then
      update-alternatives --install /usr/bin/x-terminal-emulator \
        x-terminal-emulator /usr/bin/sampa 50
    fi
    if command -v update-desktop-database >/dev/null 2>&1; then
      update-desktop-database -q /usr/share/applications || true
    fi
    ;;
esac

exit 0
