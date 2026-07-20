//! Temperature Measurement (0x0402) for the inverter's internal temperature
//! (`device/temperature`). Min/Max advertise a fixed 0 °C–80 °C sensing envelope.

use core::cell::RefCell;

use rs_matter::dm::clusters::decl::temperature_measurement as tm;
use rs_matter::dm::{Cluster, Dataver, ReadContext};
use rs_matter::error::Error;
use rs_matter::tlv::Nullable;
use rs_matter::with;

use crate::domain::PvSnapshot;

/// Advertised sensing envelope for the inverter's internal temperature,
/// in hundredths of °C (cluster resolution 0.01 °C). Lower bound: 0 °C.
const TEMP_MIN_CENTI_C: i16 = 0;
/// Upper bound of the advertised sensing envelope: 80 °C.
const TEMP_MAX_CENTI_C: i16 = 8_000;

pub struct TemperatureHandler<'a> {
    dataver: Dataver,
    state: &'a RefCell<PvSnapshot>,
}

impl<'a> TemperatureHandler<'a> {
    pub fn new(dataver: Dataver, state: &'a RefCell<PvSnapshot>) -> Self {
        Self { dataver, state }
    }
}

impl tm::ClusterHandler for TemperatureHandler<'_> {
    const CLUSTER: Cluster<'static> = tm::FULL_CLUSTER.with_attrs(with!(required));

    fn dataver(&self) -> u32 {
        self.dataver.get()
    }

    fn dataver_changed(&self) {
        self.dataver.changed();
    }

    fn measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<i16>, Error> {
        Ok(match self.state.borrow().temperature_centi_c {
            Some(v) => Nullable::some(v),
            None => Nullable::none(),
        })
    }

    fn min_measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<i16>, Error> {
        Ok(Nullable::some(TEMP_MIN_CENTI_C))
    }

    fn max_measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<i16>, Error> {
        Ok(Nullable::some(TEMP_MAX_CENTI_C))
    }
}
