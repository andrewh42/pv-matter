## MQTT contract

> Copied from the publisher's own `docs/architecture.md` (`sma-daemon`, "MQTT contract"
> section) — that document is authoritative; re-sync this file when the publisher changes.

The daemon connects as an MQTT v5 client (`rumqttc::v5`). All publishes are QoS 1, retained,
with payload-format-indicator = UTF-8 and a content-type property: `application/senml+json`
for `instantaneous`, `application/json` for `info`, `text/plain` for `status`. Message expiry
is deliberately **not** set: retained energy totals must survive the night; freshness is
signalled by the pack's base time and the `status` topic.

**Topic root and device id.** Every topic is rooted at `<prefix>/<device-id>/`, where `prefix`
is `MQTT_TOPIC_PREFIX` (default `pv-inverter`) and `device-id` is `sma-<model-slug>-<serial>` —
the model slug is the discovered inverter name truncated at its first hyphen and lowercased
(`"SB3000TL-21"` → `sb3000tl`), falling back to `sma-<serial>` if no name was discovered. The
MQTT client id is `sma-daemon-<serial>`.

**The publisher connects lazily.** The device id is baked into every topic, the LWT, and the
client id — fixed for the life of the connection — so the publisher waits until identity is
known before connecting at all: normally the first `Info` message (after the first complete
poll lap), or an `Asleep` message if the inverter goes quiet before ever completing one. On
every (re)connect it publishes `online` and re-publishes the last `info` and `instantaneous`
payloads, bridging broker restarts.

### `<prefix>/<device-id>/info` — retained JSON

Static identity plus the structural facts SenML can't carry; published on the first poll lap
of each session:

```json
{ "schema": 1, "serial": 2130012345, "name": "SB5000TL-21", "lines": 1, "strings": 2,
  "max_power_watts": { "l1": 5000 }, "device_type": "pv_inverter",
  "device_model_code": 9076, "device_model": "SB5000TL-21", "daemon_version": "0.1.0" }
```

- `schema` versions the whole topic contract (SenML packs have no envelope for it); it bumps
  on any breaking change, including a record rename.
- `lines`/`strings` are discovered from the first full sample. SenML has no `null`, so
  consumers distinguish "line exists but currently unavailable" from "doesn't exist" by
  comparing a pack against these counts.
- `device_type` is the resolved name for the LRI `0x821f` code (`device_name_map.rs`; omitted
  when the code is unrecognised or absent). `device_model_code` is the raw LRI `0x8220` code,
  always published when discovered; `device_model` appears alongside it only when the curated
  map has an entry. Full model-name resolution needs a tag dictionary (e.g. SBFspot's
  `TagList*.txt`) that is CC BY-NC-SA and cannot be ported into this Apache-2.0 project, so
  the map is curated entry by entry as devices are encountered.
- `name`, `max_power_watts` and the identity fields are omitted entirely when never
  discovered.
- The document maps roughly 1:1 onto the `device` block of a Home Assistant MQTT-discovery
  config, if that is ever wanted. If the daemon is retargeted to a different inverter, clear
  the old topics with a zero-length retained publish.

### `<prefix>/<device-id>/instantaneous` — retained SenML pack

One RFC 8428 pack per poll. A dedicated header record leads the array, carrying only the base
fields (`bn` = `urn:dev:ser:<serial>:`, `bt` = poll unix time, `bu` = `"W"`) and no `n`/`v` of
its own; every reading is an ordinary record whose full name is `bn`+`n`. Watts is by far the
most common unit, so `bu` declares it as the pack default and watt records omit `u`
(RFC 8428 §4.2); every other unit is explicit. Units are SenML registry codes (`W`, `V`, `A`,
`Hz`, `s`, `Cel`) plus `Wh` and `%` from the RFC 8798 secondary registry. Enum attributes are
string-valued records (`vs`), each drawn from a fixed value set so consumers can match on it
without a fallback numeric parse:

| Record | Enumerated values (`vs`) |
|---|---|
| `device/status` | `ok`, `warning`, `fault`, `off`, `unknown` |
| `device/grid_relay` | `open`, `closed`, `fault`, `unknown` |

`unknown` is published rather than omitting the record — the SMA-native "no data" tag code, seen
in practice on `device/grid_relay` once the inverter has shut down for the night. A code outside
either set (an id the decoder doesn't yet recognise) still passes through as the raw numeric
code rendered as a string, so a wrong assumption degrades to an unfamiliar value rather than a
crash — consumers should treat any `vs` outside the table above as unmapped, not invalid.
Numeric values are serialised as **exact decimal literals** (integer/string arithmetic, never
`f64` maths), so a reading's decimal digits — including trailing zeros reflecting its
precision — survive byte-for-byte:

```json
[
  {"bn": "urn:dev:ser:2130012345:", "bt": 1751625000, "bu": "W"},
  {"n": "ac/total_power",                        "v": 2450},
  {"n": "ac/l1/power",                           "v": 2450},
  {"n": "ac/l1/voltage",            "u": "V",    "v": 236.40},
  {"n": "ac/l1/current",            "u": "A",    "v": 10.370},
  {"n": "ac/frequency",             "u": "Hz",   "v": 49.98},
  {"n": "dc/s1/power",              "t": -10,    "v": 1310},
  {"n": "dc/s1/voltage",  "u": "V", "t": -10,    "v": 322.50},
  {"n": "dc/s2/power",              "t": -10,    "v": 1265},
  {"n": "energy/total",             "u": "Wh",   "v": 41784321},
  {"n": "energy/today",             "u": "Wh",   "v": 10432},
  {"n": "counters/operating_time",  "u": "s",    "v": 123456789},
  {"n": "device/temperature",       "u": "Cel",  "v": 41.30},
  {"n": "device/status",                         "vs": "ok"},
  {"n": "device/grid_relay",                     "vs": "closed"},
  {"n": "device/bluetooth_signal",  "u": "%",    "v": 78.4}
]
```

Rules: the pack's base time is the **latest** inverter timestamp across the readings; readings
whose own timestamp differs (DC and AC groups can resolve to different inverter timestamps)
expose the difference as a per-record `t` offset rather than hiding it. Unavailable attributes
(`0x80000000`) are omitted from the pack; unsupported (`0xffffffff`) are never published;
nothing is invented. Record name segments use complete quantity words, no unit abbreviations;
index segments follow the IEC convention — `l1`/`l2`/`l3` for AC lines (phases), `s1`/`s2` for
DC strings.

### `<prefix>/<device-id>/status` — retained text

`online` on session-open and at sunrise; `asleep` at sunset (or, without coordinates, when
the inverter stops answering); the LWT sets `offline` if the daemon dies, and a clean shutdown
publishes `offline` deliberately before disconnecting. On a broker reconnect the publisher
republishes the *current* status rather than a blanket `online`, so a broker restart during
the night cannot wake the retained status.

### What pv-matter consumes

Of the above, this bridge reads `ac/total_power`, `ac/l1/voltage`, `ac/l1/current`,
`ac/frequency`, `energy/total`, `device/temperature`, `device/status`, `device/grid_relay`
(`domain.rs::apply_pack`), plus the `status` topic and `info`'s `serial` / `device_model` /
`max_power_watts`. Everything else in the pack — `ac/l1/power`, `dc/s*`, `energy/today`,
`counters/*`, `device/bluetooth_signal` — is parsed but unmapped, and rides along for Home
Assistant, logging and debugging. See `docs/architecture.md` for why (`energy/today` has no
home in EEM without PERE; the DC strings are a deferred extension point).

