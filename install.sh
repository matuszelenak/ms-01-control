#!/usr/bin/env bash
# Build the .deb and install it.
set -euo pipefail
cd "$(dirname "$0")"

./package.sh
deb=$(ls -t server/target/debian/ms01ctl_*.deb | head -1)
apt install --yes "./$deb"

echo "ms01ctl running — UI at http://$(hostname -I | awk '{print $1}'):8090"
echo "to remove: apt remove ms01ctl   (see UNINSTALL.md)"
