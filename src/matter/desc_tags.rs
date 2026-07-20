//! Descriptor extensions the stock [`DescHandler`] doesn't cover:
//!
//! - [`SolarPartsMatcher`] — the Solar Power endpoint is a *composition
//!   parent*: its `PartsList` names the component endpoints (Power Source,
//!   Electrical Sensor, Temperature Sensor), while the root keeps the
//!   standard all-endpoints view and leaves stay empty.
//! - [`TaggedDescHandler`] — a thin wrapper adding the `TagList` attribute
//!   (feature `TAGLIST`), which the Device Library mandates on the Power
//!   Source endpoint (Grid tag) and on any Temperature Sensor (a tag naming
//!   what is measured). Everything else delegates to the wrapped handler.

use rs_matter::dm::clusters::decl::descriptor as desc;
use rs_matter::dm::clusters::decl::globals::{
    SemanticTagStructArrayBuilder, SemanticTagStructBuilder,
};
use rs_matter::dm::clusters::desc::{DescHandler, PartsMatcher};
use rs_matter::dm::{ArrayAttributeRead, Cluster, Dataver, EndptId, ReadContext};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::tlv::{Nullable, TLVBuilderParent};
use rs_matter::utils::sync::DynBase;
use rs_matter::with;

/// One Descriptor `SemanticTagStruct`.
pub struct SemanticTag {
    /// `null` = a standard namespace; `Some(vid)` = manufacturer-defined.
    pub mfg_code: Option<u16>,
    pub namespace_id: u8,
    pub tag: u8,
    pub label: Option<&'static str>,
}

/// Power Source namespace (0x0F), Grid tag (0x01) — mandated for the Power
/// Source endpoint of a Solar Power device (§14.3.6.4).
pub const POWER_SOURCE_NAMESPACE: u8 = 0x0F;
pub const POWER_SOURCE_TAG_GRID: u8 = 0x01;

/// Common Location namespace (0x06), Inside tag (0x02) — "located inside the
/// equipment" (Standard Namespaces §7). Names what a Temperature Sensor
/// measures when the sensor is internal to the device (§14.3.6.4).
pub const COMMON_LOCATION_NAMESPACE: u8 = 0x06;
pub const COMMON_LOCATION_TAG_INSIDE: u8 = 0x02;

/// PartsList matcher for the Solar Power composition endpoint: its parts are
/// the component endpoints handed in at construction.
#[derive(Debug)]
pub struct SolarPartsMatcher {
    pub parent: EndptId,
    pub children: &'static [EndptId],
}

impl DynBase for SolarPartsMatcher {}

impl PartsMatcher for SolarPartsMatcher {
    fn matches(&self, our_endpoint: EndptId, endpoint: EndptId) -> bool {
        our_endpoint == self.parent && self.children.contains(&endpoint)
    }
}

/// Descriptor with a static `TagList`, delegating everything else to the
/// wrapped [`DescHandler`].
pub struct TaggedDescHandler {
    inner: DescHandler<'static>,
    tags: &'static [SemanticTag],
}

impl TaggedDescHandler {
    pub fn new(dataver: Dataver, tags: &'static [SemanticTag]) -> Self {
        Self {
            inner: DescHandler::new(dataver),
            tags,
        }
    }
}

fn write_tag<P: TLVBuilderParent + core::fmt::Debug>(
    builder: SemanticTagStructBuilder<P>,
    tag: &SemanticTag,
) -> Result<P, Error> {
    builder
        .mfg_code(match tag.mfg_code {
            Some(vid) => Nullable::some(vid),
            None => Nullable::none(),
        })?
        .namespace_id(tag.namespace_id)?
        .tag(tag.tag)?
        .label(tag.label.map(Nullable::some))?
        .end()
}

impl desc::ClusterHandler for TaggedDescHandler {
    const CLUSTER: Cluster<'static> = desc::FULL_CLUSTER
        .with_features(desc::Feature::TAG_LIST.bits())
        .with_attrs(with!(required; desc::AttributeId::TagList));

    fn dataver(&self) -> u32 {
        desc::ClusterHandler::dataver(&self.inner)
    }

    fn dataver_changed(&self) {
        desc::ClusterHandler::dataver_changed(&self.inner)
    }

    fn device_type_list<P: TLVBuilderParent>(
        &self,
        ctx: impl ReadContext,
        builder: ArrayAttributeRead<
            desc::DeviceTypeStructArrayBuilder<P>,
            desc::DeviceTypeStructBuilder<P>,
        >,
    ) -> Result<P, Error> {
        self.inner.device_type_list(ctx, builder)
    }

    fn server_list<P: TLVBuilderParent>(
        &self,
        ctx: impl ReadContext,
        builder: ArrayAttributeRead<
            rs_matter::tlv::ToTLVArrayBuilder<P, u32>,
            rs_matter::tlv::ToTLVBuilder<P, u32>,
        >,
    ) -> Result<P, Error> {
        self.inner.server_list(ctx, builder)
    }

    fn client_list<P: TLVBuilderParent>(
        &self,
        ctx: impl ReadContext,
        builder: ArrayAttributeRead<
            rs_matter::tlv::ToTLVArrayBuilder<P, u32>,
            rs_matter::tlv::ToTLVBuilder<P, u32>,
        >,
    ) -> Result<P, Error> {
        self.inner.client_list(ctx, builder)
    }

    fn parts_list<P: TLVBuilderParent>(
        &self,
        ctx: impl ReadContext,
        builder: ArrayAttributeRead<
            rs_matter::tlv::ToTLVArrayBuilder<P, u16>,
            rs_matter::tlv::ToTLVBuilder<P, u16>,
        >,
    ) -> Result<P, Error> {
        self.inner.parts_list(ctx, builder)
    }

    fn tag_list<P: TLVBuilderParent>(
        &self,
        _ctx: impl ReadContext,
        builder: ArrayAttributeRead<SemanticTagStructArrayBuilder<P>, SemanticTagStructBuilder<P>>,
    ) -> Result<P, Error> {
        match builder {
            ArrayAttributeRead::ReadAll(mut builder) => {
                for tag in self.tags {
                    builder = write_tag(builder.push()?, tag)?;
                }
                builder.end()
            }
            ArrayAttributeRead::ReadOne(index, builder) => {
                let Some(tag) = self.tags.get(index as usize) else {
                    return Err(ErrorCode::ConstraintError.into());
                };
                write_tag(builder, tag)
            }
            ArrayAttributeRead::ReadNone(builder) => builder.end(),
        }
    }
}
