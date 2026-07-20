# pv-matter — architecture

`pv-matter` is a Rust daemon that subscribes to the `sma-daemon` MQTT topics
(see [`mqtt-contract.md`](../mqtt-contract.md)) and exposes the PV inverter on the
local Matter fabric as a **Solar Power device** (Matter 1.5.1 Device Library §14.3,
device type `0x0017`). It is built on **rs-matter** and **rumqttc v0.25**
(`rumqttc::v5` client).

It is a **read-only bridge**: data flows one way, MQTT → Matter attribute reports.
The only inbound Matter interaction is commissioning plus reads/subscriptions — there
is no command path back to the inverter.

The Rust side pulls in **no TLS stack** (plain TCP to a LAN broker) and crypto is
rs-matter's default pure-Rust `rustcrypto`; the only C linkage is `libdns_sd` for mDNS
(system mDNSResponder on macOS, avahi-compat on Linux — the bundle ships the `.so`). It
cross-compiles for aarch64 Linux via `packaging/build-aarch64.sh` (cargo-zigbuild).

> The rs-matter wiring patterns (threading model, handler-chain structure, structural
> tests) follow the proven `d26i-matter` service; this bridge is simpler because it has
> no device-command path.

---

## 1. Matter data model

### Endpoint topology

A single Matter node (**not** a bridge/aggregator), composed per Device Library §14.3.6.
Defined statically as [`NODE`](../src/matter/node.rs) and mirrored by
[`dm_handler`](../src/matter/node.rs) — the two must agree endpoint-by-endpoint, which
the structural tests in `node.rs` guard.

| EP | Device type | Server clusters | Notes |
|----|-------------|-----------------|-------|
| 0 | Root Node | `root_endpoint!(eth)` (rs-matter standard set) | Ethernet commissioning |
| 1 | Solar Power `0x0017` rev 1 | Descriptor (PartsList = EP2–4), Identify | Composition parent |
| 2 | Power Source `0x0011` | Descriptor (**TagList**: PowerSource ns `0x0F` / Grid `0x01`), Identify, Power Source (feature **WIRED**) | Grid tag is mandatory (§14.3.6.4) |
| 3 | Electrical Sensor `0x0510` | Descriptor, Identify, Power Topology (**NODE**), Electrical Power Measurement (**ALTC**), Electrical Energy Measurement (**EXPE + CUME**) | Measures the AC grid connection point |
| 4 | Temperature Sensor `0x0302` | Descriptor (**TagList**: Common Location ns `0x06` / Inside `0x02`), Identify, Temperature Measurement | `device/temperature` = the inverter's internal temperature |

Mechanics:

- rs-matter's `DescHandler` takes an endpoint **matcher** for PartsList; the composed
  hierarchy (EP1 parts = EP2–4) is expressed via `SolarPartsMatcher` in
  [`desc_tags.rs`](../src/matter/desc_tags.rs) without patching rs-matter.
- `TagList` is not surfaced by the stock `DescHandler`, so a thin **`TaggedDescHandler`**
  wrapper (also in `desc_tags.rs`) delegates everything else and serves a static
  `TagList`. It rides the Power Source and Temperature endpoints.
- Endpoint IDs are `const`s in `node.rs` (`EP_SOLAR_POWER`=1 … `EP_TEMPERATURE`=4);
  device types are `DeviceType { dtype, drev }` consts (not predefined in rs-matter's
  `dm::devices` at the pinned rev, all revision 1 in 1.5.1).

### Cluster feature/attribute selection and spec rationale

**Electrical Power Measurement `0x0090`** (Application Cluster §2.13), EP3 —
[`epm.rs`](../src/matter/epm.rs), feature **ALTERNATING_CURRENT**:

| Attribute | Source | Conformance |
|---|---|---|
| `PowerMode` | constant `AC` | M |
| `NumberOfMeasurementTypes` | constant | M |
| `Accuracy` | `MeasurementAccuracyStruct` list, one row per measurement type (4), flat ±1 %, `measured = true` | M |
| `Voltage` | `ac/l1/voltage` → mV | **M by Solar Power override** (§14.3.6.3) |
| `ActiveCurrent` | `ac/l1/current` → mA, **sign-flipped** | **M by Solar Power override** |
| `ActivePower` | `ac/total_power` → mW, **sign-flipped** | M |
| `Frequency` | `ac/frequency` → mHz | optional `[ALTC]`; we have the data |
| `ReactivePower` | — omitted | SHOULD, but the SMA protocol doesn't provide it; nothing is invented |

Sign convention (§2.13.6.6/.9): *positive = imported into the server, negative =
exported.* A generating inverter therefore reports **negative** `ActivePower` and
`ActiveCurrent`. The domain layer stores the published (production-positive) values; the
EPM handler applies the flip at read time.

The `Accuracy` envelope is not entirely constant: the ActivePower and ActiveCurrent ranges
are sized from the nameplate rating (`info.max_power_watts`, summed across lines) plus 10 %
headroom, falling back to −30 kW / −30 A until `info` arrives; Voltage (230 V ± 45 V) and
Frequency (50 Hz ± 5 Hz) are fixed brackets around nominal mains. `epm.rs::
active_power_current_min` is shared with the bridge's startup logging so the two can't drift.

**Electrical Energy Measurement `0x0091`** (§2.12), EP3 — [`eem.rs`](../src/matter/eem.rs),
features **ExportedEnergy + CumulativeEnergy**:

- The cluster demands at least one of CUME/PERE (`O.b+`). `energy/total` is a lifetime
  counter, so **CUME** is the honest choice; PERE would require inventing measurement
  periods.
- `CumulativeEnergyExported` = `EnergyMeasurementStruct { energy: energy/total → mWh,
  endTimestamp: pack base-time + record offset }`. `startTimestamp` omitted (lifetime
  counter, reset time unknown). SenML times are unix-epoch, Matter's `epoch-s` is
  2000-epoch (Core §7.19.2.4) — `eem.rs::to_epoch_s` rebases by `MATTER_EPOCH_SECS`,
  and drops instants outside the representable range rather than reporting them wrong.
- **`CumulativeEnergyMeasured` event** (M with CUME) is emitted by the bridge whenever
  the lifetime total moves, carrying the same struct (same serializer, no drift — see
  `bridge.rs::emit_cumulative_energy` / `eem.rs::write_energy_measurement`).
- `energy/today` is not mapped (nothing in EEM matches "since midnight" without PERE).

**Power Topology `0x009C`**, EP3 — [`power_topology.rs`](../src/matter/power_topology.rs),
feature **NODE**: this sensor measures the whole node's flow at the AC connection point.
No non-global attributes needed.

**Power Source `0x002F`**, EP2 — [`power_source.rs`](../src/matter/power_source.rs),
feature **WIRED**: `Status` (live, from the availability mapping below), `Order`,
`Description`, `WiredCurrentType` = AC, and `EndpointList` = `POWERED_ENDPOINTS`
(`node.rs`, EP1–4 — everything this source powers).

**Temperature Measurement `0x0402`**, EP4 —
[`temperature.rs`](../src/matter/temperature.rs): `MeasuredValue` = `device/temperature`
(Cel → centi-°C), null when absent/asleep. `MinMeasuredValue`/`MaxMeasuredValue`
advertise a fixed **0 °C–80 °C** sensing envelope (named consts `TEMP_MIN_CENTI_C` /
`TEMP_MAX_CENTI_C`).

**Quiet-reporting (Q quality).** EPM/EEM measurement attributes must not be marked
ready-for-report more than once per second. Reports are driven by inbound MQTT packs
(poll cadence ≥ seconds apart) and datavers bump only on value change, so the rule holds
structurally; the bridge additionally enforces a 1 s minimum spacing
(`MIN_REPORT_SPACING`) as a guard against burst replays. No keep-alive timer is needed.

### Basic Information / identity

Built by `main.rs::basic_info` from the bootstrap `info` document:
`ProductName` ← `info.device_model` (e.g. `SB5000TL-21`), `SerialNumber` ← `info.serial`,
`VendorName` = `"pv-matter (unofficial)"`, `SoftwareVersionString` = crate version.
Commissioned as a **test device** (`TEST_VID` `0xFFF1` / `TEST_DEV_COMM`); real CSA
certification is out of scope. These fields are `'static` (leaked once for the process
lifetime) because `Matter` borrows them for as long as it runs.

---

## 2. MQTT ingest

[`ingest.rs`](../src/ingest.rs) — an `rumqttc::v5::AsyncClient` over plain TCP, clean
start, QoS 1. Pure transport + topic routing; payload interpretation lives in `domain.rs`.

- Subscribes to `<prefix>/+/info`, `<prefix>/+/instantaneous`, `<prefix>/+/status`
  (prefix from `PV_MQTT_TOPIC_PREFIX`, default `pv-inverter`), re-subscribing on every
  ConnAck.
- The publisher's device id is self-assigned, so the `Router` **locks onto the first
  device id seen** unless `PV_DEVICE_ID` pins one; other device ids are then ignored
  (warn once).
- All three topics are retained, so state replays on every (re)connect — reconnection
  needs no special logic beyond rumqttc's event loop.
- Emits `DomainMsg::{Info, Pack, Status}` onto an mpsc channel.

**Startup bootstrap** (`main.rs`): before constructing the Matter node, fold messages
into `State` until the retained `info` arrives or `PV_BOOTSTRAP_TIMEOUT` (default 30 s)
elapses. On timeout, start anyway with placeholder identity and all measurements null —
the daemon must come up for commissioning even against an empty broker.

### SenML number handling

[`senml.rs`](../src/senml.rs). The publisher serializes exact decimal literals, and every
Matter target is a fixed-point decimal shift of the SenML unit — W→mW, V→mV, A→mA,
Hz→mHz, Wh→mWh (×1000), Cel→centi-°C (×100). So parsing is a **digit shift on the raw
JSON number text → `i64`** (`serde_json` with `arbitrary_precision`), never through
`f64`. This is lossless by construction. Core helper: `scale_decimal(s, exp) ->
Option<i64>`, unit-tested against the contract's examples.

### Availability semantics

Derived in `domain.rs::PvSnapshot::source_status` from link status, the inverter's
self-reported `device/status`, and `device/grid_relay`:

| Condition | Effect |
|---|---|
| `status` = `online` | PowerSource `Status = Active`; measurements as reported |
| `status` = `asleep` | `Status = Standby`; instantaneous EPM readings + Temperature → **null**; cumulative energy **retained** |
| `status` = `offline` (LWT) | `Status = Unavailable`; instantaneous → null; energy retained |
| record absent from a pack | that attribute → null |
| `device/grid_relay` (online) | `closed` → Active baseline; `open`/`unknown` → Standby; `fault` → Unavailable |
| `device/status` (online) | `ok`/`warning` → Active baseline; `off`/`fault` → Unavailable; `unknown` abstains |

When online, the `device/status` and `grid_relay` votes are combined **by severity** —
`SourceStatus` derives `Ord` in severity order (`Unspecified < Active < Standby <
Unavailable`), so `source_status` takes the `max`. Absent or abstaining signals leave the
`Active` baseline; any `vs` outside the contract set is left unmapped rather than guessed.

---

## 3. Runtime architecture

Single binary crate (`pv-matter`); module boundaries give separation without a workspace
split. Module layout:

```
src/
  main.rs        composition root: config → bootstrap → spawn → shutdown
  config.rs      env / .env parsing (PV_* vars), owns all defaults
  senml.rs       SenML pack parsing + exact decimal scaling (no domain knowledge)
  domain.rs      PvSnapshot, InverterInfo, State, DomainMsg — pure types + diffing
  ingest.rs      rumqttc task: topics → DomainMsg → mpsc channel
  matter/
    mod.rs           run(): builds the Matter stack, runs its event loops on the thread
    node.rs          NODE metadata tree + endpoint ids + dm_handler() chain + structural tests
    bridge.rs        applies snapshots: per-attribute change notify + EEM event
    epm.rs           ElectricalPowerMeasurement handler (sign-flip at read time)
    eem.rs           ElectricalEnergyMeasurement handler + energy-struct serializer
    power_source.rs  Power Source (WIRED) handler
    power_topology.rs Power Topology (NODE) handler
    temperature.rs   Temperature Measurement handler (0–80 °C envelope)
    desc_tags.rs     TaggedDescHandler + SolarPartsMatcher + namespace/tag consts
```

### Threading model & data flow (one-directional)

```
rumqttc event loop ──> ingest task ──> mpsc<DomainMsg> ──> main loop ──> async_channel<PvSnapshot> ──> bridge
   (tokio)             parse+lock-on                       State.apply()   (rs-matter thread)          │
                                                                                                       ├─ diff prev/next → per-attr notify_attr_changed
                                                                                                       └─ cumulative-energy change → emit_event(...)
                                                                                          RefCell<PvSnapshot> ← read by cluster handlers
```

- **Two execution worlds.** The **tokio** runtime hosts the MQTT ingest and the state
  accumulator; **rs-matter runs under `block_on` on a dedicated OS thread** (4 MiB stack,
  because the composed handler-tuple future is large — hence `#![recursion_limit = "512"]`
  in `main.rs`). They communicate only through channels.
- The main loop folds each `DomainMsg` into `State` and forwards the resulting
  `PvSnapshot` over a **bounded** `async_channel`. It blocks (briefly) rather than
  dropping newest state if the bridge is waiting out its report floor.
- Inside the rs-matter thread, `mod.rs::run` composes transport, mDNS, the Interaction
  Model responder, and the bridge into one `block_on` `select`. Cluster handlers read a
  shared `RefCell<PvSnapshot>` — sound because everything on that thread is
  single-threaded. Both the fabric store and the Interaction Model state are loaded from
  (and persisted to) the `DirKvBlobStore`, so subscriptions survive a restart
  (`persistent-subscriptions`).
- **Liveness**: an exit-guard channel lets the tokio side notice a dead Matter thread
  (error, return, or panic); likewise a dead ingest task is fatal. Shutdown
  (SIGINT/SIGTERM) signals the Matter thread and joins it with a 5 s timeout.

### Changed-attributes-only reporting

Matter's reporting granularity is the per-cluster dataver. `bridge.rs` diffs the previous
snapshot against the incoming one field-by-field and calls `notify_attr_changed` for
**only** the attributes whose backing values actually moved; unchanged clusters generate
no report, and a pack that repeats all values (e.g. retained replay after a broker
restart) generates nothing. Diffing is on the already-scaled integer fields, so "changed"
is exact, not float-fuzzy. `PvSnapshot` derives `PartialEq` for the fast `next == prev`
short-circuit.

### Design notes (SOLID, minimally)

- *Single responsibility*: `senml.rs` knows RFC 8428 but not PV; `domain.rs` knows PV but
  neither MQTT nor Matter; each handler serves exactly one cluster.
- *Open/closed*: adding endpoints (see deferred work) adds handlers + `NODE` rows; the
  ingest/diff pipeline is untouched.
- *Dependency inversion*: `bridge.rs` depends on `domain::PvSnapshot` only — replaying
  packs in tests needs no Matter code.
- No plugin registries, dyn-trait buses, or async-trait abstractions: one snapshot, one
  diff, a handful of channels.

---

## 4. Configuration

Env vars (plus `.env` in CWD; real env takes precedence). Parsed by
[`config.rs`](../src/config.rs), which owns every default.

| Var | Default | Purpose |
|---|---|---|
| `PV_MQTT_HOST` | — (**required**) | broker address |
| `PV_MQTT_PORT` | `1883` | broker port |
| `PV_MQTT_USERNAME` / `PV_MQTT_PASSWORD` | unset | optional broker auth |
| `PV_MQTT_TOPIC_PREFIX` | `pv-inverter` | matches sma-daemon's `MQTT_TOPIC_PREFIX` |
| `PV_DEVICE_ID` | unset (lock onto first seen) | pin a specific `<device-id>` |
| `PV_STORAGE_PATH` | `~/.pv-matter` | rs-matter `DirKvBlobStore` (fabric persistence) |
| `PV_MATTER_PORT` | `5540` (`MATTER_PORT`) | Matter operational UDP port, advertised over mDNS. Override to run a second daemon on one host |
| `PV_BOOTSTRAP_TIMEOUT` | `30` | seconds to wait for retained `info` at startup |
| `RUST_LOG` | `info` | `env_logger` filter |

---

## 5. Dependencies & platform

Pinned in [`Cargo.toml`](../Cargo.toml):

- **rs-matter** — git pin on the `andrewh42/rs-matter` fork (`rev 4dffc315…`, carrying the
  same Apple-Home pairing fix `d26i` ships), features `astro-dnssd`, `max-sessions-32`,
  `persistent-subscriptions`, `case-resumption`. Crypto is the default pure-Rust
  `rustcrypto`.
- **rumqttc 0.25**, `default-features = false` — drops the whole TLS/aws-lc stack; plain
  TCP to a LAN broker keeps the binary pure Rust + libc.
- **tokio** (`rt-multi-thread`, `macros`, `signal`, `time`, `sync`), **serde** /
  **serde_json** (`arbitrary_precision`), **async-channel** / **async-io** (rs-matter-side
  channels + async UDP), plus `embassy-futures`, `embassy-time-queue-utils`
  (`generic-queue-64`, supplies the timer-queue symbol the final binary must link),
  `futures-lite`, `static_cell`, `rand`, `dotenvy`, `log`/`env_logger`, `anyhow`.

**mDNS via Bonjour / DNS-SD (astro-dnssd) on both platforms.** macOS links the system
mDNSResponder; Linux links `libdns_sd` from **avahi-compat** (`libavahi-compat-libdnssd`),
which speaks avahi-daemon's *legacy* D-Bus API — chosen because the alternative `zbus`
backend only talks to `org.freedesktop.Avahi.Server2` (Avahi ≥ 0.8), which the Bionic
target (Avahi 0.7) does not ship. **Runtime requirement: a running `avahi-daemon` on the
target box.** The C linkage this pulls in on Linux is handled by `build-aarch64.sh`.

Packaging (`packaging/`): glibc 2.27 pin, cargo-zigbuild cross-build, tar.gz + sha256
bundle, `install.sh` / `INSTALL.md`. UDP 5540 (operational) + mDNS 5353 must be reachable
on the LAN.

---

## 6. Testing

- **Unit**: `senml.rs` decimal scaling + pack parsing against the contract's example pack;
  `domain.rs` diffing (change / no-change / null transitions) and the availability
  mapping as table-driven tests; `ingest.rs` topic routing / device-id lock-on;
  `eem.rs` epoch rebasing (including out-of-range instants).
- **Structural** (`node.rs` tests): assert the `NODE` metadata tree and cluster feature
  maps agree endpoint-by-endpoint with what the handlers serve — the guard that catches
  metadata/handler drift at build time (feature-map bits, mandated EPM attributes present,
  ReactivePower/CumulativeEnergyImported honestly absent, tags correct).
- **End-to-end (manual)**: local mosquitto replaying retained sample payloads; commission
  into Apple Home / chip-tool (see [`README-chip-tool`](../README-chip-tool)); verify
  reports arrive only on value changes, nulls appear on `asleep`, and the
  `CumulativeEnergyMeasured` event fires on total-energy movement.

---

## 7. Deferred / extension points

Clean extension points exist for these; none are implemented today:

- **Per-string DC endpoints** — config-gated child Electrical Sensors (DIRC EPM:
  `dc/sX/power`, `dc/sX/voltage`), User Label, Common-Number tags, TreeTopology. The
  snapshot/diff pipeline already generalizes; this adds handlers + `NODE` rows only.
- **Polyphase inverters** — the contract can carry `l1..l3`; a `lines > 1` build would set
  EPM feature **POLY** and report per-phase values (same child-Electrical-Sensor
  mechanism).
- **`energy/today` via PERE**, **ReactivePower** (needs upstream data),
  **Device Energy Management** (mandated only for controllable output — this bridge is
  read-only), and **production DAC/PAA certificates** (test VID/PID today).
