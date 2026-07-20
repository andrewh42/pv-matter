#!/usr/bin/env bash
# Installs pv-matter on the target box: binary + run wrapper into
# /usr/local/sbin, systemd unit, and /etc/pv-matter/config.env (prompted for
# on first install). Run as root from inside the unpacked bundle directory.
set -euo pipefail

if [[ "$(id -u)" -ne 0 ]]; then
  echo "error: run as root (sudo ./install.sh)" >&2
  exit 1
fi

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONFIG_DIR=/etc/pv-matter
CONFIG="${CONFIG_DIR}/config.env"

# mDNS goes through avahi-daemon over D-Bus; without it the Matter side cannot
# advertise itself and commissioning will fail.
if ! systemctl is-active --quiet avahi-daemon 2>/dev/null; then
  echo ">> avahi-daemon is not running — installing/enabling it"
  apt-get install -y avahi-daemon
  systemctl enable --now avahi-daemon
fi

echo ">> installing binary and wrapper"
install -m 0755 "${HERE}/pv-matter" /usr/local/sbin/pv-matter
install -m 0755 "${HERE}/pv-matter-run" /usr/local/sbin/pv-matter-run

echo ">> installing systemd unit"
install -m 0644 "${HERE}/pv-matter.service" /etc/systemd/system/pv-matter.service

MQTT_USERNAME=pv-matter

if [[ ! -f "${CONFIG}" ]]; then
  echo ">> creating ${CONFIG}"
  read -r -p "MQTT broker host [localhost]: " mqtt_host
  mqtt_host="${mqtt_host:-localhost}"
  read -r -p "MQTT broker port [1883]: " mqtt_port
  mqtt_port="${mqtt_port:-1883}"
  read -r -s -p "MQTT broker password for user '${MQTT_USERNAME}' (empty = anonymous): " mqtt_password; echo

  mkdir -p "${CONFIG_DIR}"
  umask 077
  cat > "${CONFIG}" <<EOF
PV_MQTT_HOST=${mqtt_host}
PV_MQTT_PORT=${mqtt_port}
EOF
  if [[ -n "${mqtt_password}" ]]; then
    cat >> "${CONFIG}" <<EOF
PV_MQTT_USERNAME=${MQTT_USERNAME}
PV_MQTT_PASSWORD=${mqtt_password}
EOF
  fi
  chmod 0600 "${CONFIG}"
else
  echo ">> keeping existing ${CONFIG}"
fi

echo ">> enabling service"
systemctl daemon-reload
systemctl enable pv-matter.service
systemctl restart pv-matter.service

echo ">> done. follow logs with: journalctl -fu pv-matter"
echo ">> on first run, the commissioning QR code appears in the journal:"
echo ">>   journalctl -u pv-matter | grep -A40 'QR'"
