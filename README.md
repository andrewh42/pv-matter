# pv-matter — MQTT PV inverter feed → Matter Solar Power device

Exposes an MQTT event stream published by a photovoltaic inverter on the local
Matter fabric — so it shows up in Apple Home (or any other Matter controller) —
by consuming the retained MQTT telemetry published by **sma-daemon** (see
`mqtt-contract.md`). Implemented in Rust on top of rs-matter.

It surfaces the Matter 1.5 **Solar Power** device type (`0x0017`), composed
per the Device Library spec:

- **Power Source** (wired, Grid semantic tag) — `Status` tracks the inverter's
  `status` topic and grid relay.
- **Electrical Sensor** — Power Topology + Electrical Power Measurement
  (ActivePower/Voltage/ActiveCurrent/Frequency, AC, export-negative sign
  convention) + Electrical Energy Measurement (lifetime exported energy, with
  the `CumulativeEnergyMeasured` event).
- **Temperature Sensor** — the inverter's internal temperature.

Only attributes whose backing values actually changed are reported to
subscribers; unavailable readings (night, publisher offline) read as null
while the retained energy total survives.

See `docs/architecture.md` for the full design.

## Configure

Environment (or `.env` in the working directory; real env takes precedence):

| Var | Default | Purpose |
|---|---|---|
| `PV_MQTT_HOST` | — (required) | broker sma-daemon publishes to |
| `PV_MQTT_PORT` | `1883` | |
| `PV_MQTT_USERNAME` / `PV_MQTT_PASSWORD` | unset | broker auth |
| `PV_MQTT_TOPIC_PREFIX` | `pv-inverter` | must match sma-daemon |
| `PV_DEVICE_ID` | first seen | pin a publisher device id |
| `PV_STORAGE_PATH` | `~/.pv-matter` | Matter fabric persistence |
| `PV_MATTER_PORT` | `5540` | Matter operational UDP port (advertised over mDNS) |
| `PV_BOOTSTRAP_TIMEOUT` | `30` | seconds to wait for retained `info` |
| `RUST_LOG` | `info` | logging |

## Build & run (macOS development)

```sh
cargo run
```

On first run the bridge is uncommissioned and prints a QR code and manual
pairing code. In the iOS Home app: *Add Accessory → More options…* and scan
the QR. It commissions as a **test** device (VID `0xFFF1`). The fabric
persists under `PV_STORAGE_PATH`; delete that directory to start over.
Ctrl-C / SIGTERM shut down cleanly.

Requires local UDP 5540 + mDNS via Bonjour / DNS-SD: macOS links the system
mDNSResponder; Linux links `libdns_sd` from avahi-compat and needs a running
`avahi-daemon`.

## Deploy (aarch64 Linux)

```sh
packaging/build-aarch64.sh        # cargo-zigbuild, glibc 2.27 floor
# → dist/pv-matter-<version>-aarch64-linux-gnu.tar.gz
```

Copy the bundle to the box and run `sudo ./install.sh` — see
`packaging/INSTALL.md`. The `--docker` fallback builds inside an arm64 Bionic
image instead.

## Tests

```sh
cargo test      # SenML/domain/router units + node structural tests
cargo clippy --all-targets
```
