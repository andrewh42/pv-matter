# Packaging

Deployment artefacts for the aarch64 Linux target (Armbian/Ubuntu 18.04
"Bionic", glibc 2.27), following the d26i-matter pattern:

- `build-aarch64.sh` — cross-build + bundle assembly; produces the install
  bundle under `dist/`. The default path uses **cargo-zigbuild** (Zig as the
  cross-linker) with the glibc symbol floor pinned to the target's 2.27
  (`--target aarch64-unknown-linux-gnu.2.27`) — the crate is pure Rust (mDNS
  talks to avahi-daemon over D-Bus via `zbus`, crypto is `rustcrypto`, rumqttc
  is built without its TLS features), so no sysroot or emulation is needed and
  the build runs at native speed. Requirements:
  `brew install zig cargo-zigbuild`.
- `Dockerfile` — the fallback build path (`build-aarch64.sh --docker`): a
  native-under-QEMU cargo build inside an arm64 Bionic image. Much slower, but
  needs only Docker; use it on hosts without Zig, or to cross-check the
  zigbuild artifact against a real Bionic userland.
- `pv-matter.service` — systemd unit (DynamicUser,
  `StateDirectory=pv-matter` for the Matter fabric, wants `avahi-daemon`).
- `pv-matter-run` — ExecStart wrapper implementing progressive restart
  backoff (15 s doubling to 1 h; a run that stays up ≥ 5 min resets it). The
  target's systemd predates `RestartSteps`/`RestartMaxDelaySec`.
- `install.sh` — ensures avahi-daemon is running, installs binary + wrapper +
  unit and prompts `/etc/pv-matter/config.env` into existence on first
  install.
- `config.env.example` / `INSTALL.md` — configuration reference and target-box
  install steps.

## Runtime requirements on the target

- **avahi-daemon** (+ D-Bus): the Matter mDNS responder registers through it
  over the system bus — no `libdns_sd`/avahi C libraries are linked.
- **UDP 5540** (Matter) and **UDP 5353** (mDNS) reachable on the LAN.
- An MQTT broker carrying sma-daemon's retained `pv-inverter/#` topics.

## Broker access for pv-matter

pv-matter is a read-only consumer of the telemetry topics. If the broker
(mosquitto, see sma-daemon's packaging docs for its full setup) uses
authentication, create a `pv-matter` user and grant it read access:

```sh
sudo mosquitto_passwd /etc/mosquitto/passwd pv-matter
```

`/etc/mosquitto/acl`:

```
user pv-matter
topic read pv-inverter/#
```

`install.sh` prompts for this password and writes the matching
`PV_MQTT_USERNAME`/`PV_MQTT_PASSWORD` lines to `/etc/pv-matter/config.env`;
leave the prompt empty only if the broker allows anonymous connections.

TLS is deliberately not configured: all traffic is LAN-local telemetry, and
authentication + ACLs are what protect topic integrity.
