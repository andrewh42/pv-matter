//! Pure PV domain: the snapshot the Matter handlers read, the inverter
//! identity, and the mapping from contract messages onto both. Knows the
//! mqtt-contract record names and units, but nothing about MQTT transport or
//! Matter encoding.
//!
//! All numeric fields are already in Matter's units (mV/mA/mW/mHz/mWh,
//! centi-°C), scaled exactly by [`crate::senml::scale_decimal`], so "changed"
//! comparisons downstream are integer-exact.

use std::collections::BTreeMap;

use crate::senml::{self, Pack, Value};

/// The `status` topic (plus "no status seen yet").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LinkStatus {
    /// Nothing received yet — before the retained topics replay.
    #[default]
    Unknown,
    Online,
    /// Civil dusk (or the inverter stopped answering): instantaneous readings
    /// are meaningless, retained energy totals are not.
    Asleep,
    /// The publisher's LWT fired or it shut down cleanly.
    Offline,
}

impl LinkStatus {
    pub fn parse(payload: &str) -> Option<Self> {
        match payload.trim() {
            "online" => Some(Self::Online),
            "asleep" => Some(Self::Asleep),
            "offline" => Some(Self::Offline),
            _ => None,
        }
    }
}

/// Static identity from the retained `info` document. Everything is optional —
/// the daemon must come up even against a broker that has never seen
/// sma-daemon — and consumers fall back to placeholders.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InverterInfo {
    pub serial: Option<u64>,
    /// User-assigned inverter name, e.g. `"SunMonkey"`. Drives the publisher's
    /// device-id slug; not the hardware model.
    pub name: Option<String>,
    /// Hardware model string, e.g. `"SB5000TL-21"`. Reported as the Matter
    /// Basic Information product name (the "Model" a controller shows).
    pub device_model: Option<String>,
    pub lines: Option<u32>,
    pub strings: Option<u32>,
    /// Nameplate AC power rating in watts. Sizes the EPM ActivePower /
    /// ActiveCurrent accuracy envelopes; not an instantaneous reading.
    pub max_power_w: Option<i64>,
}

impl InverterInfo {
    /// Parses the retained `info` JSON. Unknown fields are ignored; a schema
    /// bump is logged but known fields are still mapped (forward
    /// compatibility beats refusing data).
    pub fn parse(payload: &[u8]) -> Result<Self, serde_json::Error> {
        #[derive(serde::Deserialize)]
        struct Raw {
            schema: Option<u32>,
            serial: Option<u64>,
            name: Option<String>,
            device_model: Option<String>,
            lines: Option<u32>,
            strings: Option<u32>,
            // Per-line nameplate ratings keyed by AC line (`{ "l1": 5000 }`);
            // the EPM accuracy envelope sizes from the whole-inverter total, so
            // the per-line values are summed at map time.
            max_power_watts: Option<BTreeMap<String, u32>>,
        }
        let raw: Raw = serde_json::from_slice(payload)?;
        if let Some(schema) = raw.schema
            && schema != 1
        {
            log::warn!("info schema {schema} != 1; mapping known fields only");
        }
        Ok(Self {
            serial: raw.serial,
            name: raw.name,
            device_model: raw.device_model,
            lines: raw.lines,
            strings: raw.strings,
            max_power_w: raw
                .max_power_watts
                .map(|per_line| per_line.values().map(|&w| i64::from(w)).sum()),
        })
    }
}

/// What the Matter Power Source cluster should say, derived from link status,
/// the inverter's self-reported `device/status`, and the grid relay. Variants
/// are declared **in severity order** so [`PvSnapshot::source_status`] can
/// combine several independent online signals by taking the most severe
/// (`max`). `Unspecified` sorts lowest and is only produced by the
/// link-`Unknown` branch, never as a per-signal vote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SourceStatus {
    Unspecified,
    Active,
    Standby,
    Unavailable,
}

/// `device/status`: the inverter's self-reported health (SenML `vs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceStatus {
    Ok,
    Warning,
    Fault,
    Off,
    Unknown,
}

impl DeviceStatus {
    /// Parses a contract `vs` value; `None` for anything outside the set (the
    /// raw-numeric-code passthrough the contract warns about is left unmapped
    /// rather than guessed at).
    fn parse(vs: &str) -> Option<Self> {
        match vs {
            "ok" => Some(Self::Ok),
            "warning" => Some(Self::Warning),
            "fault" => Some(Self::Fault),
            "off" => Some(Self::Off),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }

    /// This signal's contribution to the online PowerSource `Status`, or `None`
    /// when it abstains (`unknown` = no self-health data).
    fn source_vote(self) -> Option<SourceStatus> {
        match self {
            Self::Ok | Self::Warning => Some(SourceStatus::Active),
            Self::Fault | Self::Off => Some(SourceStatus::Unavailable),
            Self::Unknown => None,
        }
    }
}

/// `device/grid_relay`: the AC grid-disconnect relay (SenML `vs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridRelay {
    Closed,
    Open,
    Fault,
    Unknown,
}

impl GridRelay {
    fn parse(vs: &str) -> Option<Self> {
        match vs {
            "closed" => Some(Self::Closed),
            "open" => Some(Self::Open),
            "fault" => Some(Self::Fault),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }

    /// This signal's contribution to the online PowerSource `Status`. `unknown`
    /// (the nightly "no data" tag) reads as `Standby` — not feeding — rather
    /// than abstaining.
    fn source_vote(self) -> Option<SourceStatus> {
        Some(match self {
            Self::Closed => SourceStatus::Active,
            Self::Open | Self::Unknown => SourceStatus::Standby,
            Self::Fault => SourceStatus::Unavailable,
        })
    }
}

/// The one shared state the Matter cluster handlers read. Instantaneous
/// fields are `None` whenever they "cannot currently be measured" (absent
/// from the pack, or the inverter is asleep/offline) — Matter reads them as
/// null. The cumulative energy total survives sleep, matching the contract's
/// retained-totals semantics.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PvSnapshot {
    pub link: LinkStatus,
    /// `ac/l1/voltage`, millivolts.
    pub voltage_mv: Option<i64>,
    /// `ac/l1/current`, milliamps, as published (production positive; the
    /// Matter layer applies the import-positive sign flip).
    pub current_ma: Option<i64>,
    /// `ac/total_power`, milliwatts, as published (production positive).
    pub power_mw: Option<i64>,
    /// `ac/frequency`, millihertz.
    pub frequency_mhz: Option<i64>,
    /// `energy/total`, milliwatt-hours (lifetime export counter).
    pub energy_total_mwh: Option<i64>,
    /// Inverter timestamp of the energy total (pack base time + record
    /// offset), unix seconds. SenML times are unix-epoch (RFC 8428 §4.5.3);
    /// the Matter layer rebases to the 2000-epoch `epoch-s` it reports in.
    pub energy_end_ts: Option<i64>,
    /// `device/temperature`, hundredths of °C.
    pub temperature_centi_c: Option<i16>,
    /// `device/status`: the inverter's self-reported health. `None` = absent
    /// from the pack, or masked because the link is not online.
    pub device_status: Option<DeviceStatus>,
    /// `device/grid_relay`: the grid-disconnect relay. `None` = absent or masked.
    pub grid_relay: Option<GridRelay>,
    /// Nameplate AC power rating in watts, from the `info` topic. Sizes the EPM
    /// ActivePower / ActiveCurrent accuracy envelopes. Nameplate identity, not
    /// an instantaneous reading — retained across sleep/offline (never masked).
    pub max_power_w: Option<i64>,
}

/// Matter milli-unit shift (×10³) for a SenML unit, or `None` if the unit is
/// one we don't map.
fn milli_shift(unit: Option<&str>) -> Option<u32> {
    match unit {
        Some("W") | Some("V") | Some("A") | Some("Hz") | Some("Wh") => Some(3),
        _ => None,
    }
}

impl PvSnapshot {
    /// Applies one `instantaneous` pack. Instantaneous readings not present
    /// in the pack become `None` (the publisher omits unavailable readings);
    /// the energy total is retained when absent.
    pub fn apply_pack(&mut self, pack: &Pack) {
        let mut voltage = None;
        let mut current = None;
        let mut power = None;
        let mut frequency = None;
        let mut temperature = None;
        let mut device_status = None;
        let mut relay = None;

        for record in &pack.records {
            match record.name.as_str() {
                "ac/total_power" => power = scaled(record, milli_shift(record.unit.as_deref())),
                "ac/l1/voltage" => voltage = scaled(record, milli_shift(record.unit.as_deref())),
                "ac/l1/current" => current = scaled(record, milli_shift(record.unit.as_deref())),
                "ac/frequency" => frequency = scaled(record, milli_shift(record.unit.as_deref())),
                "device/temperature" => {
                    temperature = match record.unit.as_deref() {
                        Some("Cel") => scaled(record, Some(2)).and_then(|v| i16::try_from(v).ok()),
                        _ => None,
                    }
                }
                "energy/total" => {
                    if let Some(mwh) = scaled(record, milli_shift(record.unit.as_deref())) {
                        self.energy_total_mwh = Some(mwh);
                        self.energy_end_ts = Some(pack.base_time + record.time_offset);
                    }
                }
                "device/status" => {
                    if let Value::Text(s) = &record.value {
                        device_status = DeviceStatus::parse(s);
                    }
                }
                "device/grid_relay" => {
                    if let Value::Text(s) = &record.value {
                        relay = GridRelay::parse(s);
                    }
                }
                _ => {}
            }
        }

        self.voltage_mv = voltage;
        self.current_ma = current;
        self.power_mw = power;
        self.frequency_mhz = frequency;
        self.temperature_centi_c = temperature;
        self.device_status = device_status;
        self.grid_relay = relay;
        self.enforce_link_mask();
    }

    /// Applies a `status` transition. Asleep/offline blank the instantaneous
    /// readings (a retained pack from before dusk must not read as live), but
    /// keep the energy total.
    pub fn set_status(&mut self, status: LinkStatus) {
        self.link = status;
        self.enforce_link_mask();
    }

    fn enforce_link_mask(&mut self) {
        if self.link != LinkStatus::Online {
            self.voltage_mv = None;
            self.current_ma = None;
            self.power_mw = None;
            self.frequency_mhz = None;
            self.temperature_centi_c = None;
            self.device_status = None;
            self.grid_relay = None;
        }
    }

    /// The Power Source cluster's `Status`, per the availability table in the
    /// implementation plan. When online the inverter's `device/status` and the
    /// grid relay are combined by severity (the most severe wins); absent or
    /// abstaining signals leave the `Active` baseline.
    pub fn source_status(&self) -> SourceStatus {
        match self.link {
            LinkStatus::Unknown => SourceStatus::Unspecified,
            LinkStatus::Offline => SourceStatus::Unavailable,
            LinkStatus::Asleep => SourceStatus::Standby,
            LinkStatus::Online => [
                self.device_status.and_then(DeviceStatus::source_vote),
                self.grid_relay.and_then(GridRelay::source_vote),
            ]
            .into_iter()
            .flatten()
            .max()
            .unwrap_or(SourceStatus::Active),
        }
    }
}

/// A number record scaled by `10^shift`, or `None` when the unit was
/// unexpected or the literal malformed. Nothing is invented.
fn scaled(record: &senml::Record, shift: Option<u32>) -> Option<i64> {
    let shift = shift?;
    match &record.value {
        Value::Number(raw) => {
            let v = senml::scale_decimal(raw, shift);
            if v.is_none() {
                log::warn!("unparseable numeric literal for {}: {raw:?}", record.name);
            }
            v
        }
        Value::Text(_) => None,
    }
}

/// One parsed contract message, transport-agnostic.
#[derive(Debug, Clone, PartialEq)]
pub enum DomainMsg {
    Info(InverterInfo),
    Pack(Pack),
    Status(LinkStatus),
}

/// Accumulates [`DomainMsg`]s into the current snapshot + identity. Owned by
/// the composition root: primed during bootstrap, then fed for the life of
/// the process.
#[derive(Debug, Clone, Default)]
pub struct State {
    pub snapshot: PvSnapshot,
    pub info: Option<InverterInfo>,
}

impl State {
    pub fn apply(&mut self, msg: DomainMsg) {
        match msg {
            DomainMsg::Info(info) => {
                // Nameplate rating is the one identity field the Matter thread
                // reads, so copy it onto the (forwarded) snapshot as well.
                self.snapshot.max_power_w = info.max_power_w;
                self.info = Some(info);
            }
            DomainMsg::Pack(pack) => self.snapshot.apply_pack(&pack),
            DomainMsg::Status(status) => self.snapshot.set_status(status),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::senml::parse_pack;

    const CONTRACT_PACK: &[u8] = br#"[
      {"bn": "urn:dev:ser:2130012345:", "bt": 1751625000, "bu": "W"},
      {"n": "ac/total_power",                        "v": 2450},
      {"n": "ac/l1/power",                           "v": 2450},
      {"n": "ac/l1/voltage",            "u": "V",    "v": 236.40},
      {"n": "ac/l1/current",            "u": "A",    "v": 10.370},
      {"n": "ac/frequency",             "u": "Hz",   "v": 49.98},
      {"n": "dc/s1/power",              "t": -10,    "v": 1310},
      {"n": "energy/total",             "u": "Wh",   "v": 41784321, "t": -10},
      {"n": "energy/today",             "u": "Wh",   "v": 10432},
      {"n": "device/temperature",       "u": "Cel",  "v": 41.30},
      {"n": "device/status",                         "vs": "ok"},
      {"n": "device/grid_relay",                     "vs": "closed"}
    ]"#;

    fn online_with_pack() -> PvSnapshot {
        let mut snap = PvSnapshot::default();
        snap.set_status(LinkStatus::Online);
        snap.apply_pack(&parse_pack(CONTRACT_PACK).unwrap());
        snap
    }

    #[test]
    fn maps_contract_pack_into_matter_units() {
        let snap = online_with_pack();
        assert_eq!(snap.power_mw, Some(2_450_000));
        assert_eq!(snap.voltage_mv, Some(236_400));
        assert_eq!(snap.current_ma, Some(10_370));
        assert_eq!(snap.frequency_mhz, Some(49_980));
        assert_eq!(snap.energy_total_mwh, Some(41_784_321_000));
        assert_eq!(snap.energy_end_ts, Some(1_751_624_990)); // bt + t(-10)
        assert_eq!(snap.temperature_centi_c, Some(4_130));
        assert_eq!(snap.device_status, Some(DeviceStatus::Ok));
        assert_eq!(snap.grid_relay, Some(GridRelay::Closed));
    }

    #[test]
    fn absent_instantaneous_readings_become_none_but_energy_is_retained() {
        let mut snap = online_with_pack();
        let sparse = br#"[
          {"bn": "urn:dev:ser:2130012345:", "bt": 1751625300, "bu": "W"},
          {"n": "ac/total_power", "v": 900}
        ]"#;
        snap.apply_pack(&parse_pack(sparse).unwrap());
        assert_eq!(snap.power_mw, Some(900_000));
        assert_eq!(snap.voltage_mv, None);
        assert_eq!(snap.frequency_mhz, None);
        assert_eq!(snap.temperature_centi_c, None);
        // The lifetime counter survives a pack that omits it.
        assert_eq!(snap.energy_total_mwh, Some(41_784_321_000));
        assert_eq!(snap.energy_end_ts, Some(1_751_624_990));
    }

    #[test]
    fn asleep_blanks_instantaneous_but_keeps_energy() {
        let mut snap = online_with_pack();
        snap.set_status(LinkStatus::Asleep);
        assert_eq!(snap.power_mw, None);
        assert_eq!(snap.voltage_mv, None);
        assert_eq!(snap.current_ma, None);
        assert_eq!(snap.frequency_mhz, None);
        assert_eq!(snap.temperature_centi_c, None);
        assert_eq!(snap.energy_total_mwh, Some(41_784_321_000));
        assert_eq!(snap.source_status(), SourceStatus::Standby);
    }

    #[test]
    fn retained_pack_replay_while_asleep_stays_masked() {
        // Broker restart at night: the retained pack replays while status is
        // asleep — instantaneous readings must not resurrect.
        let mut snap = PvSnapshot::default();
        snap.set_status(LinkStatus::Asleep);
        snap.apply_pack(&parse_pack(CONTRACT_PACK).unwrap());
        assert_eq!(snap.power_mw, None);
        assert_eq!(snap.energy_total_mwh, Some(41_784_321_000));
    }

    #[test]
    fn source_status_table() {
        let mut snap = PvSnapshot::default();
        assert_eq!(snap.source_status(), SourceStatus::Unspecified);
        snap.set_status(LinkStatus::Online);
        assert_eq!(snap.source_status(), SourceStatus::Active);
        snap.grid_relay = Some(GridRelay::Open);
        assert_eq!(snap.source_status(), SourceStatus::Standby);
        snap.set_status(LinkStatus::Offline);
        assert_eq!(snap.source_status(), SourceStatus::Unavailable);
    }

    #[test]
    fn grid_relay_maps_by_state_while_online() {
        let mut snap = PvSnapshot::default();
        snap.set_status(LinkStatus::Online);
        snap.grid_relay = Some(GridRelay::Closed);
        assert_eq!(snap.source_status(), SourceStatus::Active);
        snap.grid_relay = Some(GridRelay::Open);
        assert_eq!(snap.source_status(), SourceStatus::Standby);
        snap.grid_relay = Some(GridRelay::Unknown);
        assert_eq!(snap.source_status(), SourceStatus::Standby);
        snap.grid_relay = Some(GridRelay::Fault);
        assert_eq!(snap.source_status(), SourceStatus::Unavailable);
    }

    #[test]
    fn device_status_maps_by_health_while_online() {
        let mut snap = PvSnapshot::default();
        snap.set_status(LinkStatus::Online);
        snap.device_status = Some(DeviceStatus::Ok);
        assert_eq!(snap.source_status(), SourceStatus::Active);
        snap.device_status = Some(DeviceStatus::Warning);
        assert_eq!(snap.source_status(), SourceStatus::Active);
        // `unknown` abstains, leaving the Active baseline.
        snap.device_status = Some(DeviceStatus::Unknown);
        assert_eq!(snap.source_status(), SourceStatus::Active);
        snap.device_status = Some(DeviceStatus::Off);
        assert_eq!(snap.source_status(), SourceStatus::Unavailable);
        snap.device_status = Some(DeviceStatus::Fault);
        assert_eq!(snap.source_status(), SourceStatus::Unavailable);
    }

    #[test]
    fn online_signals_combine_by_severity() {
        let mut snap = PvSnapshot::default();
        snap.set_status(LinkStatus::Online);
        // A healthy device with an open relay is Standby (relay wins).
        snap.device_status = Some(DeviceStatus::Ok);
        snap.grid_relay = Some(GridRelay::Open);
        assert_eq!(snap.source_status(), SourceStatus::Standby);
        // A faulted device with a closed relay is Unavailable (fault wins).
        snap.grid_relay = Some(GridRelay::Closed);
        snap.device_status = Some(DeviceStatus::Fault);
        assert_eq!(snap.source_status(), SourceStatus::Unavailable);
    }

    #[test]
    fn device_status_parses_contract_values_only() {
        let mut snap = PvSnapshot::default();
        snap.set_status(LinkStatus::Online);
        // An unrecognised vs (e.g. a raw numeric-code passthrough) is left
        // unmapped, not guessed at — it abstains like `unknown`.
        let odd = br#"[
          {"bn": "urn:dev:ser:1:", "bt": 1, "bu": "W"},
          {"n": "device/status", "vs": "12345"},
          {"n": "device/grid_relay", "vs": "12345"}
        ]"#;
        snap.apply_pack(&parse_pack(odd).unwrap());
        assert_eq!(snap.device_status, None);
        assert_eq!(snap.grid_relay, None);
        assert_eq!(snap.source_status(), SourceStatus::Active);
    }

    #[test]
    fn unexpected_units_are_skipped_not_misscaled() {
        let mut snap = PvSnapshot::default();
        snap.set_status(LinkStatus::Online);
        let odd = br#"[
          {"bn": "urn:dev:ser:1:", "bt": 1, "bu": "W"},
          {"n": "ac/l1/voltage", "u": "kV", "v": 0.23},
          {"n": "ac/total_power", "v": 100}
        ]"#;
        snap.apply_pack(&parse_pack(odd).unwrap());
        assert_eq!(snap.voltage_mv, None);
        assert_eq!(snap.power_mw, Some(100_000));
    }

    #[test]
    fn parses_info_document() {
        let info = InverterInfo::parse(
            br#"{ "schema": 1, "serial": 2130012345, "name": "SunMonkey",
                  "device_model": "SB5000TL-21", "lines": 1, "strings": 2,
                  "max_power_watts": { "l1": 5000 }, "device_type": "pv_inverter" }"#,
        )
        .unwrap();
        assert_eq!(info.serial, Some(2_130_012_345));
        assert_eq!(info.name.as_deref(), Some("SunMonkey"));
        assert_eq!(info.device_model.as_deref(), Some("SB5000TL-21"));
        assert_eq!(info.lines, Some(1));
        assert_eq!(info.strings, Some(2));
        assert_eq!(info.max_power_w, Some(5000));
    }

    #[test]
    fn info_nameplate_rating_sums_per_line_ratings() {
        // A three-phase inverter reports one rating per line; the EPM total
        // envelope is the whole-inverter sum.
        let info = InverterInfo::parse(
            br#"{ "max_power_watts": { "l1": 5000, "l2": 5000, "l3": 5000 } }"#,
        )
        .unwrap();
        assert_eq!(info.max_power_w, Some(15_000));
    }

    #[test]
    fn info_nameplate_rating_reaches_the_snapshot() {
        let info = InverterInfo::parse(br#"{ "max_power_watts": { "l1": 5000 } }"#).unwrap();
        let mut state = State::default();
        state.apply(DomainMsg::Info(info));
        // The rating is nameplate identity: it lands on the forwarded snapshot
        // and survives an offline transition (never masked).
        assert_eq!(state.snapshot.max_power_w, Some(5000));
        state.apply(DomainMsg::Status(LinkStatus::Offline));
        assert_eq!(state.snapshot.max_power_w, Some(5000));
    }

    #[test]
    fn status_parses_contract_values_only() {
        assert_eq!(LinkStatus::parse("online"), Some(LinkStatus::Online));
        assert_eq!(LinkStatus::parse("asleep\n"), Some(LinkStatus::Asleep));
        assert_eq!(LinkStatus::parse("offline"), Some(LinkStatus::Offline));
        assert_eq!(LinkStatus::parse("bogus"), None);
    }
}
