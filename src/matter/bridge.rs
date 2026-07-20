//! The MQTT → Matter fan-out: consumes snapshot updates, applies them to the
//! shared state, and notifies **exactly the attributes whose backing values
//! moved** (`notify_attr_changed` is per-attribute, so unchanged attributes
//! are never re-reported — subscription reports carry only real changes).
//!
//! Also owns the EEM `CumulativeEnergyMeasured` event: mandatory with the
//! CUME feature, emitted when the lifetime total moves.
//!
//! Quiet-reporting guard: EPM/EEM measurement attributes must not be marked
//! ready-for-report more than once per second. Updates arrive at the
//! publisher's poll cadence (many seconds), so the guard is a fallback: if a
//! burst arrives (e.g. replayed packs), we wait out the remainder and
//! coalesce to the newest snapshot before reporting.

use core::cell::RefCell;
use std::time::{Duration, Instant};

use rs_matter::dm::clusters::decl::electrical_energy_measurement as eem;
use rs_matter::dm::clusters::decl::electrical_power_measurement as epm;
use rs_matter::dm::clusters::decl::power_source as ps;
use rs_matter::dm::clusters::decl::temperature_measurement as tm;
use rs_matter::dm::{AttrChangeNotifier, EventEmitter};
use rs_matter::error::Error;
use rs_matter::im::{EventDataTag, EventPriority};
use rs_matter::tlv::{TLVTag, TLVWrite};

use crate::domain::PvSnapshot;

use super::eem::write_energy_measurement;
use super::node::{EP_ELECTRICAL_SENSOR, EP_POWER_SOURCE, EP_TEMPERATURE};

/// Spec floor for Q-quality attributes (EPM §2.13.6.x, EEM §2.12.6.x).
const MIN_REPORT_SPACING: Duration = Duration::from_secs(1);

/// Runs until the update channel closes (app shutdown).
pub async fn run_bridge(
    state: &RefCell<PvSnapshot>,
    dm: &(impl AttrChangeNotifier + EventEmitter),
    updates: &async_channel::Receiver<PvSnapshot>,
) -> Result<(), Error> {
    let mut prev = state.borrow().clone();
    let mut last_report: Option<Instant> = None;

    // Log the accuracy minimums the EPM handler will report for the initial
    // rating (fallback bounds when no nameplate has arrived yet).
    log_accuracy_mins(prev.max_power_w);

    loop {
        let Ok(mut next) = updates.recv().await else {
            return Ok(());
        };

        // Quiet-reporting floor: wait out the remainder of the 1 s window…
        if let Some(last) = last_report {
            let elapsed = last.elapsed();
            if elapsed < MIN_REPORT_SPACING {
                async_io::Timer::after(MIN_REPORT_SPACING - elapsed).await;
            }
        }
        // …and coalesce whatever queued up meanwhile to the newest snapshot.
        while let Ok(newer) = updates.try_recv() {
            next = newer;
        }

        if next == prev {
            continue;
        }
        *state.borrow_mut() = next.clone();

        // Electrical Power Measurement: the four live readings.
        for (changed, attr) in [
            (
                next.voltage_mv != prev.voltage_mv,
                epm::AttributeId::Voltage,
            ),
            (
                next.current_ma != prev.current_ma,
                epm::AttributeId::ActiveCurrent,
            ),
            (
                next.power_mw != prev.power_mw,
                epm::AttributeId::ActivePower,
            ),
            (
                next.frequency_mhz != prev.frequency_mhz,
                epm::AttributeId::Frequency,
            ),
        ] {
            if changed {
                dm.notify_attr_changed(EP_ELECTRICAL_SENSOR, epm::FULL_CLUSTER.id, attr as u32);
            }
        }

        // Electrical Energy Measurement: report + event when the lifetime
        // total moves. (A new inverter timestamp with an unchanged total is
        // not a change worth reporting — nothing the consumer acts on moved.)
        if next.energy_total_mwh != prev.energy_total_mwh {
            dm.notify_attr_changed(
                EP_ELECTRICAL_SENSOR,
                eem::FULL_CLUSTER.id,
                eem::AttributeId::CumulativeEnergyExported as u32,
            );
            if let Some(mwh) = next.energy_total_mwh {
                emit_cumulative_energy(dm, mwh, next.energy_end_ts);
            }
        }

        // Power Source: Status is the only live attribute.
        if next.source_status() != prev.source_status() {
            dm.notify_attr_changed(
                EP_POWER_SOURCE,
                ps::FULL_CLUSTER.id,
                ps::AttributeId::Status as u32,
            );
        }

        if next.temperature_centi_c != prev.temperature_centi_c {
            dm.notify_attr_changed(
                EP_TEMPERATURE,
                tm::FULL_CLUSTER.id,
                tm::AttributeId::MeasuredValue as u32,
            );
        }

        // The nameplate rating drives the EPM ActivePower/ActiveCurrent accuracy
        // minimums; log the new bounds whenever it moves.
        if next.max_power_w != prev.max_power_w {
            log_accuracy_mins(next.max_power_w);
        }

        prev = next;
        last_report = Some(Instant::now());
    }
}

/// Logs the ActivePower/ActiveCurrent accuracy lower bounds the EPM handler
/// reports for the given nameplate rating (production-negative, so both ≤ 0).
fn log_accuracy_mins(max_power_w: Option<i64>) {
    let (power_min_mw, current_min_ma) = super::epm::active_power_current_min(max_power_w);
    log::info!(
        "EPM accuracy minimums (nameplate {max_power_w:?} W): ActivePower {power_min_mw} mW, ActiveCurrent {current_min_ma} mA"
    );
}

/// Emits `CumulativeEnergyMeasured` with an `energyExported` field mirroring
/// the `CumulativeEnergyExported` attribute (same serialiser, no drift).
fn emit_cumulative_energy(dm: &impl EventEmitter, energy_mwh: i64, end_unix_s: Option<i64>) {
    let result = dm.emit_event(
        EP_ELECTRICAL_SENSOR,
        eem::FULL_CLUSTER.id,
        eem::EventId::CumulativeEnergyMeasured as _,
        EventPriority::Info,
        |mut tw| {
            // The closure writes the EventDataIB `Data` field (tag 7): the event
            // payload is itself a struct whose field 1 is EnergyExported (field
            // 0, EnergyImported, is absent — this device only exports). Writing
            // the EnergyMeasurementStruct straight in with tag 1 collides with
            // EventDataTag::EventNumber, so rs-matter re-parses it as a u64 and
            // panics (im/events.rs `EventData::from_tlv`).
            tw.start_struct(&TLVTag::Context(EventDataTag::Data as _))?;
            let builder = eem::EnergyMeasurementStructBuilder::new(tw, &TLVTag::Context(1))?;
            let mut tw = write_energy_measurement(builder, energy_mwh, end_unix_s)?;
            tw.end_container()?;
            Ok(())
        },
    );
    if let Err(e) = result {
        log::warn!("failed to emit CumulativeEnergyMeasured: {e}");
    }
}
