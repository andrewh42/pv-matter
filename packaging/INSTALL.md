# Installing pv-matter

Built for aarch64 Linux (Armbian/Ubuntu 18.04 "Bionic", glibc 2.27).

Prerequisites on the target box:

- **avahi-daemon** running (mDNS; the installer enables it if missing).
- **libavahi-compat-libdnssd1** installed — provides `libdns_sd.so.1`, which the
  binary links for mDNS (`apt-get install libavahi-compat-libdnssd1`).
- The MQTT broker sma-daemon publishes to, reachable from this box.
- LAN reachability for Matter: **UDP 5540** (Matter) and **UDP 5353** (mDNS)
  must not be firewalled between this box and your Matter controller
  (e.g. an Apple Home hub on the same LAN).

1. Copy the bundle to the target box and unpack it:

   ```sh
   tar xzf pv-matter-<version>-aarch64-linux-gnu.tar.gz
   cd pv-matter-<version>-aarch64-linux-gnu
   ```

2. Run the installer as root. On first install it prompts for the MQTT broker
   host/port and the broker password for the `pv-matter` user (leave empty for
   an anonymous, localhost-only broker), and writes them to
   `/etc/pv-matter/config.env` (mode 0600):

   ```sh
   sudo ./install.sh
   ```

3. Commission into your Matter controller. On first run the daemon prints a
   QR code and manual pairing code to the journal:

   ```sh
   journalctl -u pv-matter | grep -B2 -A40 'Manual pairing code'
   ```

   In the iOS Home app: *Add Accessory → More options…* and scan the QR (or
   enter the code). It commissions as a **test** device (VID `0xFFF1`).

4. Verify:

   ```sh
   journalctl -fu pv-matter
   ```

The service unit runs as a dynamic user and persists the Matter fabric under
`/var/lib/pv-matter` (delete that directory to reset commissioning), restarting
with progressive backoff via `pv-matter-run`.

Broker-side setup (the `pv-matter` broker user needs read access to
`pv-inverter/#`) is described in `README.md` alongside this file.
