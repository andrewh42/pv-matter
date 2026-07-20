# CLAUDE.md

Guidance for working in this repository.

## What this is

`pv-matter` is a **read-only** Rust daemon that bridges an SMA PV inverter from MQTT onto
the local Matter fabric as a **Solar Power device** (Matter 1.5.1 §14.3, device type
`0x0017`). Data flows one way: MQTT (`sma-daemon` topics) → Matter attribute reports.
Built on **rs-matter** (git pin) and **rumqttc v0.25** (`v5` client).

## Architecture

Full detail in **[docs/architecture.md](docs/architecture.md)** — read it before making
structural changes. Key pointers:

- **Endpoint topology** (`src/matter/node.rs`): a single node, not a bridge. EP0 Root,
  EP1 Solar Power (composition parent), EP2 Power Source (WIRED + Grid tag), EP3
  Electrical Sensor (Power Topology / EPM / EEM), EP4 Temperature Sensor. `NODE` (metadata)
  and `dm_handler` (handler chain) **must stay in agreement** — the structural tests in
  `node.rs` guard this; update both together.
- **Threading**: tokio runtime hosts MQTT ingest + state accumulation; **rs-matter runs
  on a dedicated OS thread under `block_on`** (`src/matter/mod.rs`). The two talk only via
  channels. Cluster handlers read a shared `RefCell<PvSnapshot>` (single-threaded on the
  rs-matter thread, so `RefCell` is sound).
- **Data flow**: `ingest.rs` → `mpsc<DomainMsg>` → `main.rs` folds into `State` →
  `async_channel<PvSnapshot>` → `bridge.rs` diffs and notifies **only changed attributes**
  (+ emits the EEM `CumulativeEnergyMeasured` event).
- **Units are exact integers**: SenML decimal text is digit-shifted to Matter milli-units
  (`src/senml.rs`, `scale_decimal`) — never `f64`. Diffing is integer-exact.
- **Sign convention**: Matter is import-positive, so a generating inverter reports
  **negative** ActivePower/ActiveCurrent. The domain stores production-positive values;
  the EPM handler flips at read time.
- **Availability**: `domain.rs::source_status` combines link status + `device/status` +
  `device/grid_relay` by severity; instantaneous readings go null when asleep/offline,
  cumulative energy is retained.
- **Layers don't leak**: `senml.rs` knows RFC 8428 not PV; `domain.rs` knows PV not
  MQTT/Matter; each handler serves one cluster.

## Conventions

- Constants are module-level `const`, `SCREAMING_SNAKE_CASE`, with the unit baked into the
  name (e.g. `TEMP_MAX_CENTI_C`, `ENERGY_MAX_MWH`), doc-commented, defined in the module
  that uses them (`pub` only when shared).
- Config is env-driven (`PV_*`, plus `.env`), all defaults owned by `src/config.rs`.

## Build & test

- `cargo build` / `cargo clippy` / `cargo test` / `cargo fmt` on stable. **After any
  change, clippy and fmt must be clean before the work is done.**
- Cross-compile for the aarch64 target with `packaging/build-aarch64.sh` (cargo-zigbuild).
  The binary is pure Rust + libc; the target needs a running **avahi-daemon** (mDNS via
  astro-dnssd / avahi-compat).
- Manual end-to-end: replay retained sample payloads into a local mosquitto, then
  commission with chip-tool (see `README-chip-tool`).
