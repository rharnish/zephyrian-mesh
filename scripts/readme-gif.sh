#!/usr/bin/env bash
# Turns a capture-mode recording (a .webm from `?capture`, key r — see
# src/captureMode.js) into a README-sized looping GIF.
#
# Usage: scripts/readme-gif.sh IN.webm OUT.gif [WIDTH] [FPS]
#   WIDTH  output width in px, height keeps the aspect ratio (default 800)
#   FPS    output frame rate (default 15)
#
# Two-pass palette (palettegen/paletteuse): a GIF gets 256 colours, and a
# palette built from this clip rather than a generic one is what keeps the
# globe from banding. Needs ffmpeg on PATH, or FFMPEG=/path/to/ffmpeg.
set -euo pipefail

if [[ $# -lt 2 ]]; then
  sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'
  exit 1
fi

in=$1
out=$2
width=${3:-800}
fps=${4:-15}
ffmpeg=${FFMPEG:-ffmpeg}

if ! command -v "$ffmpeg" >/dev/null; then
  echo "ffmpeg not found — sudo apt install ffmpeg (or set FFMPEG=...)" >&2
  exit 1
fi

filters="fps=${fps},scale=${width}:-1:flags=lanczos"
"$ffmpeg" -hide_banner -loglevel error -y -i "$in" -vf \
  "${filters},split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle" \
  -loop 0 "$out"

echo "$out: $(du -h "$out" | cut -f1)"
