//! Electrical Power Measurement (0x0090) over the shared snapshot: the AC
//! grid connection point of the inverter.
//!
//! Feature `ALTC` (AC-wired per Device Library §14.3.6.3), plus the
//! Solar-Power-mandated `Voltage`/`ActiveCurrent` and the optional
//! `Frequency` (we have the reading; `ReactivePower` is a SHOULD the SMA
//! Bluetooth protocol cannot supply, so it is honestly absent).
//!
//! Sign convention (spec §2.13.6): positive = imported into the server — a
//! generating inverter therefore reports **negative** ActivePower and
//! ActiveCurrent. The snapshot stores production-positive values; the flip
//! happens here, at the Matter boundary.

use core::cell::RefCell;

use rs_matter::dm::clusters::decl::electrical_power_measurement as epm;
use rs_matter::dm::clusters::decl::globals::{
    MeasurementAccuracyStructArrayBuilder, MeasurementAccuracyStructBuilder, MeasurementTypeEnum,
};
use rs_matter::dm::{ArrayAttributeRead, Cluster, Dataver, ReadContext};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::im::{AmperageMilliA, PowerMilliW, VoltageMilliV};
use rs_matter::tlv::{Nullable, TLVBuilderParent};
use rs_matter::with;

use crate::domain::PvSnapshot;

/// One row of the mandatory `Accuracy` list. The SMA protocol publishes no
/// accuracy metadata, so these are conservative constants: the measurement
/// envelope plus a flat ±1 % claim (`measured = true` — the inverter measures,
/// it does not estimate).
struct AccuracyEntry {
    measurement_type: MeasurementTypeEnum,
    min: i64,
    max: i64,
}

/// ±1 % in hundredths of a percent.
const PERCENT_MAX: u16 = 100;

/// Nominal mains frequency in milli-hertz (50 Hz). The Frequency accuracy
/// envelope brackets this by ±5 Hz.
const MAINS_FREQUENCY_MHZ: i64 = 50_000;

/// Nominal mains voltage in millivolts (230 V). The Voltage accuracy envelope
/// brackets this by ±45 V; `VOLTAGE_MIN_MV` is also the divisor that turns the
/// power rating into the ActiveCurrent bound, so both derive from here.
const NOMINAL_MAINS_VOLTAGE_MV: i64 = 230_000;
/// Voltage envelope half-width, millivolts (±45 V).
const VOLTAGE_TOLERANCE_MV: i64 = 45_000;
/// Low end of the Voltage envelope (185 V) and the divisor for the current bound.
const VOLTAGE_MIN_MV: i64 = NOMINAL_MAINS_VOLTAGE_MV - VOLTAGE_TOLERANCE_MV;
/// High end of the Voltage envelope (275 V).
const VOLTAGE_MAX_MV: i64 = NOMINAL_MAINS_VOLTAGE_MV + VOLTAGE_TOLERANCE_MV;

/// Milliwatts of envelope headroom per nameplate watt: ×1.1 (10 % margin) then
/// ×1000 (W → mW), exact with no rounding.
const POWER_HEADROOM_MW_PER_W: i64 = 1_100;

/// The Solar-Power measurement types this server reports accuracy for
/// (spec §2.13.6.3 requires an entry for ActivePower and every implemented
/// type). Fixed count, per `NumberOfMeasurementTypes` (§2.13.6.2).
const MEASUREMENT_TYPES: usize = 4;

/// Fallback ActivePower lower bound (−30 kW, mW) used until the nameplate rating
/// arrives on the `info` topic. The `Accuracy` list must always carry all rows
/// (they cannot be omitted), so this keeps a valid, generous envelope.
const DEFAULT_ACTIVE_POWER_MIN_MW: i64 = -30_000_000;
/// Fallback ActiveCurrent lower bound (−30 A, mA), see [`DEFAULT_ACTIVE_POWER_MIN_MW`].
const DEFAULT_ACTIVE_CURRENT_MIN_MA: i64 = -30_000;

/// The ActivePower (mW) and ActiveCurrent (mA) accuracy lower bounds. Sized from
/// the nameplate rating `max_power_w` (watts) when known — production is negative
/// per the sign convention, so both are ≤ 0 — else the `DEFAULT_*` fallbacks.
/// Shared by [`accuracy_entries`] and the bridge's INFO logging so the two never
/// drift.
pub fn active_power_current_min(max_power_w: Option<i64>) -> (i64, i64) {
    match max_power_w {
        Some(w) => {
            // mW = W × 1.1 × 1000; mA = mW × 1000 ÷ mV. Both export-negative.
            let power_mw = w * POWER_HEADROOM_MW_PER_W;
            let current_ma = power_mw * 1_000 / VOLTAGE_MIN_MV;
            (-power_mw, -current_ma)
        }
        None => (DEFAULT_ACTIVE_POWER_MIN_MW, DEFAULT_ACTIVE_CURRENT_MIN_MA),
    }
}

/// Builds the `Accuracy` envelope for all measurement types. ActivePower and
/// ActiveCurrent are sized from the nameplate rating `max_power_w` (watts) when
/// known — production is negative per the sign convention — else fall back to
/// the `DEFAULT_*` bounds. Voltage and Frequency are fixed brackets around the
/// nominal mains values.
fn accuracy_entries(max_power_w: Option<i64>) -> [AccuracyEntry; MEASUREMENT_TYPES] {
    let (power_min, current_min) = active_power_current_min(max_power_w);
    [
        AccuracyEntry {
            measurement_type: MeasurementTypeEnum::ActivePower,
            min: power_min, // … 0 (export only)
            max: 0,
        },
        AccuracyEntry {
            measurement_type: MeasurementTypeEnum::Voltage,
            min: VOLTAGE_MIN_MV, // 185 … 275 V (230 V ± 45 V)
            max: VOLTAGE_MAX_MV,
        },
        AccuracyEntry {
            measurement_type: MeasurementTypeEnum::ActiveCurrent,
            min: current_min, // … 0 (export only)
            max: 0,
        },
        AccuracyEntry {
            measurement_type: MeasurementTypeEnum::Frequency,
            min: MAINS_FREQUENCY_MHZ - 5_000, // 45 … 55 Hz (50 Hz ± 5 Hz)
            max: MAINS_FREQUENCY_MHZ + 5_000,
        },
    ]
}

pub struct EpmHandler<'a> {
    dataver: Dataver,
    state: &'a RefCell<PvSnapshot>,
}

impl<'a> EpmHandler<'a> {
    pub fn new(dataver: Dataver, state: &'a RefCell<PvSnapshot>) -> Self {
        Self { dataver, state }
    }
}

fn nullable(value: Option<i64>) -> Nullable<i64> {
    match value {
        Some(v) => Nullable::some(v),
        None => Nullable::none(),
    }
}

/// Writes one Accuracy entry through the type-state struct builder.
fn write_accuracy<P: TLVBuilderParent + core::fmt::Debug>(
    builder: MeasurementAccuracyStructBuilder<P>,
    entry: &AccuracyEntry,
) -> Result<P, Error> {
    builder
        .measurement_type(entry.measurement_type)?
        .measured(true)?
        .min_measured_value(entry.min)?
        .max_measured_value(entry.max)?
        .accuracy_ranges()?
        .push()?
        .range_min(entry.min)?
        .range_max(entry.max)?
        .percent_max(Some(PERCENT_MAX))?
        .percent_min(None)?
        .percent_typical(None)?
        .fixed_max(None)?
        .fixed_min(None)?
        .fixed_typical(None)?
        .end()?
        .end()?
        .end()
}

impl epm::ClusterHandler for EpmHandler<'_> {
    const CLUSTER: Cluster<'static> = epm::FULL_CLUSTER
        .with_features(epm::Feature::ALTERNATING_CURRENT.bits())
        .with_attrs(with!(
            required;
            epm::AttributeId::Voltage
                | epm::AttributeId::ActiveCurrent
                | epm::AttributeId::Frequency
        ));

    fn dataver(&self) -> u32 {
        self.dataver.get()
    }

    fn dataver_changed(&self) {
        self.dataver.changed();
    }

    fn power_mode(&self, _ctx: impl ReadContext) -> Result<epm::PowerModeEnum, Error> {
        Ok(epm::PowerModeEnum::AC)
    }

    fn number_of_measurement_types(&self, _ctx: impl ReadContext) -> Result<u8, Error> {
        Ok(MEASUREMENT_TYPES as u8)
    }

    fn accuracy<P: TLVBuilderParent>(
        &self,
        _ctx: impl ReadContext,
        builder: ArrayAttributeRead<
            MeasurementAccuracyStructArrayBuilder<P>,
            MeasurementAccuracyStructBuilder<P>,
        >,
    ) -> Result<P, Error> {
        let entries = accuracy_entries(self.state.borrow().max_power_w);
        match builder {
            ArrayAttributeRead::ReadAll(mut builder) => {
                for entry in &entries {
                    builder = write_accuracy(builder.push()?, entry)?;
                }
                builder.end()
            }
            ArrayAttributeRead::ReadOne(index, builder) => {
                let Some(entry) = entries.get(index as usize) else {
                    return Err(ErrorCode::ConstraintError.into());
                };
                write_accuracy(builder, entry)
            }
            ArrayAttributeRead::ReadNone(builder) => builder.end(),
        }
    }

    fn voltage(&self, _ctx: impl ReadContext) -> Result<Nullable<VoltageMilliV>, Error> {
        Ok(nullable(self.state.borrow().voltage_mv))
    }

    fn active_current(&self, _ctx: impl ReadContext) -> Result<Nullable<AmperageMilliA>, Error> {
        // Import-positive: production flows out of the server → negative.
        Ok(nullable(self.state.borrow().current_ma.map(|ma| -ma)))
    }

    fn active_power(&self, _ctx: impl ReadContext) -> Result<Nullable<PowerMilliW>, Error> {
        Ok(nullable(self.state.borrow().power_mw.map(|mw| -mw)))
    }

    fn frequency(&self, _ctx: impl ReadContext) -> Result<Nullable<i64>, Error> {
        Ok(nullable(self.state.borrow().frequency_mhz))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(entries: &[AccuracyEntry], ty: MeasurementTypeEnum) -> &AccuracyEntry {
        entries
            .iter()
            .find(|e| e.measurement_type == ty)
            .expect("measurement type present")
    }

    #[test]
    fn rating_sizes_power_and_current_bounds() {
        let entries = accuracy_entries(Some(5000));
        // 5000 W × 1.1 = 5500 W → −5_500_000 mW; ÷ 185 V → −29_729 mA (trunc).
        assert_eq!(
            entry(&entries, MeasurementTypeEnum::ActivePower).min,
            -5_500_000
        );
        assert_eq!(
            entry(&entries, MeasurementTypeEnum::ActiveCurrent).min,
            -29_729
        );
    }

    #[test]
    fn voltage_brackets_mains_and_current_uses_the_low_end() {
        let entries = accuracy_entries(Some(5000));
        let voltage = entry(&entries, MeasurementTypeEnum::Voltage);
        assert_eq!((voltage.min, voltage.max), (185_000, 275_000));
        // ActiveCurrent divides by exactly this low end.
        assert_eq!(voltage.min, VOLTAGE_MIN_MV);
    }

    #[test]
    fn missing_rating_falls_back_to_defaults() {
        let entries = accuracy_entries(None);
        assert_eq!(
            entry(&entries, MeasurementTypeEnum::ActivePower).min,
            DEFAULT_ACTIVE_POWER_MIN_MW
        );
        assert_eq!(
            entry(&entries, MeasurementTypeEnum::ActiveCurrent).min,
            DEFAULT_ACTIVE_CURRENT_MIN_MA
        );
    }

    #[test]
    fn every_measurement_type_is_export_or_bracketed() {
        // Spec §2.13.6.3: an entry for ActivePower and every implemented type.
        let entries = accuracy_entries(Some(5000));
        assert_eq!(entries.len(), MEASUREMENT_TYPES);
        // Power/current are export-only (≤ 0); voltage/frequency are positive.
        assert!(entry(&entries, MeasurementTypeEnum::ActivePower).max == 0);
        assert!(entry(&entries, MeasurementTypeEnum::ActiveCurrent).max == 0);
    }
}
