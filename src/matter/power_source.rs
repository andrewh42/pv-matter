//! Power Source (0x002F) for the Power Source component endpoint: feature
//! `WIRED` (the inverter feeds premises wiring). `Status` tracks the link,
//! the inverter's `device/status`, and the grid relay (see
//! [`PvSnapshot::source_status`]); the rest is static.

use core::cell::RefCell;

use rs_matter::dm::clusters::decl::power_source as ps;
use rs_matter::dm::{ArrayAttributeRead, Cluster, Dataver, EndptId, ReadContext};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::tlv::{TLVBuilderParent, ToTLVArrayBuilder, ToTLVBuilder, Utf8StrBuilder};
use rs_matter::with;

use crate::domain::{PvSnapshot, SourceStatus};

pub struct PowerSourceHandler<'a> {
    dataver: Dataver,
    state: &'a RefCell<PvSnapshot>,
    /// `EndpointList`: the endpoints this source powers — the device's
    /// functional endpoints.
    powered_endpoints: &'static [EndptId],
}

impl<'a> PowerSourceHandler<'a> {
    pub fn new(
        dataver: Dataver,
        state: &'a RefCell<PvSnapshot>,
        powered_endpoints: &'static [EndptId],
    ) -> Self {
        Self {
            dataver,
            state,
            powered_endpoints,
        }
    }
}

fn to_matter(status: SourceStatus) -> ps::PowerSourceStatusEnum {
    match status {
        SourceStatus::Unspecified => ps::PowerSourceStatusEnum::Unspecified,
        SourceStatus::Active => ps::PowerSourceStatusEnum::Active,
        SourceStatus::Standby => ps::PowerSourceStatusEnum::Standby,
        SourceStatus::Unavailable => ps::PowerSourceStatusEnum::Unavailable,
    }
}

impl ps::ClusterHandler for PowerSourceHandler<'_> {
    const CLUSTER: Cluster<'static> = ps::FULL_CLUSTER
        .with_features(ps::Feature::WIRED.bits())
        .with_attrs(with!(required; ps::AttributeId::WiredCurrentType));

    fn dataver(&self) -> u32 {
        self.dataver.get()
    }

    fn dataver_changed(&self) {
        self.dataver.changed();
    }

    fn status(&self, _ctx: impl ReadContext) -> Result<ps::PowerSourceStatusEnum, Error> {
        Ok(to_matter(self.state.borrow().source_status()))
    }

    fn order(&self, _ctx: impl ReadContext) -> Result<u8, Error> {
        // The single (and thus preferred) source of this node.
        Ok(0)
    }

    fn description<P: TLVBuilderParent>(
        &self,
        _ctx: impl ReadContext,
        builder: Utf8StrBuilder<P>,
    ) -> Result<P, Error> {
        builder.set("PV inverter (grid)")
    }

    fn wired_current_type(
        &self,
        _ctx: impl ReadContext,
    ) -> Result<ps::WiredCurrentTypeEnum, Error> {
        Ok(ps::WiredCurrentTypeEnum::AC)
    }

    fn endpoint_list<P: TLVBuilderParent>(
        &self,
        _ctx: impl ReadContext,
        builder: ArrayAttributeRead<ToTLVArrayBuilder<P, EndptId>, ToTLVBuilder<P, EndptId>>,
    ) -> Result<P, Error> {
        match builder {
            ArrayAttributeRead::ReadAll(mut builder) => {
                for ep in self.powered_endpoints {
                    builder = builder.push(ep)?;
                }
                builder.end()
            }
            ArrayAttributeRead::ReadOne(index, builder) => {
                let Some(ep) = self.powered_endpoints.get(index as usize) else {
                    return Err(ErrorCode::ConstraintError.into());
                };
                builder.set(ep)
            }
            ArrayAttributeRead::ReadNone(builder) => builder.end(),
        }
    }
}
