#!/bin/sh
# Debian pre-remove: withdraw the terminal alternative registered by postinst.
set -e

case "$1" in
  remove|deconfigure)
    if command -v update-alternatives >/dev/null 2>&1; then
      update-alternatives --remove x-terminal-emulator /usr/bin/sampa || true
    fi
    ;;
esac

exit 0
