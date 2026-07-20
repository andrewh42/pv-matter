//! Electrical Energy Measurement (0x0091): the inverter's lifetime export
//! counter (`energy/total`).
//!
//! Features `EXPE` (mandated by the Solar Power device type) + `CUME` (the
//! cluster requires CUME or PERE; `energy/total` is a lifetime counter, so
//! cumulative is the honest fit). With CUME the `CumulativeEnergyMeasured`
//! event is mandatory — emitted by the bridge whenever the total moves.
//!
//! `CumulativeEnergyExported` carries the energy in mWh plus the inverter's
//! own timestamp as `endTimestamp`; `startTimestamp` (when the counter last
//! reset) is unknowable for a lifetime counter and legitimately omitted.

use core::cell::RefCell;

use rs_matter::dm::clusters::decl::electrical_energy_measurement as eem;
use rs_matter::dm::clusters::decl::globals::{
    MeasurementAccuracyStructBuilder, MeasurementTypeEnum,
};
use rs_matter::dm::{Cluster, Dataver, ReadContext};
use rs_matter::error::Error;
use rs_matter::tlv::{NullableBuilder, TLVBuilderParent};
use rs_matter::utils::epoch::MATTER_EPOCH_SECS;
use rs_matter::with;

use crate::domain::PvSnapshot;

/// Envelope for the mandatory Accuracy attribute: 0 … 10⁹ Wh in mWh (no
/// accuracy metadata exists on the SMA side; `measured = true`).
const ENERGY_MIN_MWH: i64 = 0;
const ENERGY_MAX_MWH: i64 = 1_000_000_000_000;
/// Fixed accuracy bound: flat ±1 kWh across the whole range, expressed in mWh
/// (fixedMax is int64u).
const ENERGY_ACCURACY_MWH: u64 = 1_000_000; // accuracy is +/- 1 mWh

/// Rebases a unix-epoch timestamp onto Matter's `epoch-s` (2000-epoch u32,
/// Core §7.19.2.4) — SenML times are unix-epoch (RFC 8428 §4.5.3), so the two
/// differ by [`MATTER_EPOCH_SECS`].
///
/// Instants before 2000 or beyond the u32 range are not representable, so the
/// timestamp is dropped rather than reported wrong — the field is optional.
fn to_epoch_s(unix_s: i64) -> Option<u32> {
    u32::try_from(unix_s.checked_sub(i64::try_from(MATTER_EPOCH_SECS).ok()?)?).ok()
}

pub struct EemHandler<'a> {
    dataver: Dataver,
    state: &'a RefCell<PvSnapshot>,
}

impl<'a> EemHandler<'a> {
    pub fn new(dataver: Dataver, state: &'a RefCell<PvSnapshot>) -> Self {
        Self { dataver, state }
    }
}

/// Writes an `EnergyMeasurementStruct` (used by both the attribute read here
/// and the bridge's event emission — one serialiser, no drift).
///
/// `end_unix_s` is the inverter's own timestamp in unix seconds; the rebase to
/// `epoch-s` happens here so both callers get it right.
pub fn write_energy_measurement<P: TLVBuilderParent + core::fmt::Debug>(
    builder: eem::EnergyMeasurementStructBuilder<P>,
    energy_mwh: i64,
    end_unix_s: Option<i64>,
) -> Result<P, Error> {
    builder
        .energy(energy_mwh)?
        .start_timestamp(None)?
        .end_timestamp(end_unix_s.and_then(to_epoch_s))?
        .start_systime(None)?
        .end_systime(None)?
        .apparent_energy(None)?
        .reactive_energy(None)?
        .end()
}

impl eem::ClusterHandler for EemHandler<'_> {
    const CLUSTER: Cluster<'static> = eem::FULL_CLUSTER
        .with_features(
            eem::Feature::EXPORTED_ENERGY
                .union(eem::Feature::CUMULATIVE_ENERGY)
                .bits(),
        )
        .with_attrs(with!(required; eem::AttributeId::CumulativeEnergyExported));

    fn dataver(&self) -> u32 {
        self.dataver.get()
    }

    fn dataver_changed(&self) {
        self.dataver.changed();
    }

    fn accuracy<P: TLVBuilderParent>(
        &self,
        _ctx: impl ReadContext,
        builder: MeasurementAccuracyStructBuilder<P>,
    ) -> Result<P, Error> {
        builder
            .measurement_type(MeasurementTypeEnum::ElectricalEnergy)?
            .measured(true)?
            .min_measured_value(ENERGY_MIN_MWH)?
            .max_measured_value(ENERGY_MAX_MWH)?
            .accuracy_ranges()?
            .push()?
            .range_min(ENERGY_MIN_MWH)?
            .range_max(ENERGY_MAX_MWH)?
            .percent_max(None)?
            .percent_min(None)?
            .percent_typical(None)?
            .fixed_max(Some(ENERGY_ACCURACY_MWH))?
            .fixed_min(None)?
            .fixed_typical(None)?
            .end()?
            .end()?
            .end()
    }

    fn cumulative_energy_exported<P: TLVBuilderParent>(
        &self,
        _ctx: impl ReadContext,
        builder: NullableBuilder<P, eem::EnergyMeasurementStructBuilder<P>>,
    ) -> Result<P, Error> {
        let (energy, end_ts) = {
            let snap = self.state.borrow();
            (snap.energy_total_mwh, snap.energy_end_ts)
        };
        match energy {
            Some(mwh) => write_energy_measurement(builder.non_null()?, mwh, end_ts),
            // "If the cumulative energy exported cannot currently be
            // determined, a value of null SHALL be returned."
            None => builder.null(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebases_unix_seconds_onto_the_matter_epoch() {
        // The Matter epoch itself.
        assert_eq!(to_epoch_s(MATTER_EPOCH_SECS as i64), Some(0));
        // The contract sample's energy timestamp: 2025-07-04T10:29:50Z.
        assert_eq!(to_epoch_s(1_751_624_990), Some(804_940_190));
    }

    #[test]
    fn unrepresentable_instants_are_dropped_not_wrapped() {
        // Before 2000: negative epoch-s has no encoding.
        assert_eq!(to_epoch_s(MATTER_EPOCH_SECS as i64 - 1), None);
        assert_eq!(to_epoch_s(0), None);
        // Beyond the u32 range (year 2136).
        assert_eq!(
            to_epoch_s(MATTER_EPOCH_SECS as i64 + i64::from(u32::MAX) + 1),
            None
        );
        assert_eq!(to_epoch_s(i64::MIN), None);
    }
}
