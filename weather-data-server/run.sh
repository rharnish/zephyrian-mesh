#!/usr/bin/env bash
# Launches wind_backend.py using the minimal venv in this directory (.venv),
# Creates the venv and installs requirements.txt on first run if it doesn't exist yet.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

if [ ! -d .venv ]; then
  echo "No .venv found — creating one and installing requirements.txt..."
  python3 -m venv .venv
  .venv/bin/pip install --upgrade pip -q
  .venv/bin/pip install -r requirements.txt -q
fi

exec .venv/bin/uvicorn wind_backend:app --host "${WIND_BACKEND_HOST:-127.0.0.1}" --port 8000 --reload
