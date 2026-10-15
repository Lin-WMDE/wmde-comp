#!/bin/sh
# Перечитывает user-юниты после установки. Ошибки не роняют установку.
set -e
command -v systemctl >/dev/null 2>&1 && systemctl --user daemon-reload || true
