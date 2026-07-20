//! The composed-device topology: Endpoint 0 (Root Node) plus the Solar Power
//! composition mandated by Device Library §14.3.6 — the Solar Power endpoint
//! itself (composition parent) with Power Source, Electrical Sensor and
//! Temperature Sensor component endpoints. A single Matter node, **not** a
//! Bridge/Aggregator.
//!
//! [`NODE`] is the static metadata tree (endpoints → device types + cluster
//! decls); [`dm_handler`] builds the matching handler chain binding our
//! cluster handlers (over the shared [`PvSnapshot`]) to each
//! endpoint+cluster. The two must agree — the structural tests guard that.

use core::cell::RefCell;

use rand::RngCore;
use rs_matter::dm::clusters::decl::descriptor;
use rs_matter::dm::clusters::decl::electrical_energy_measurement as eem;
use rs_matter::dm::clusters::decl::electrical_power_measurement as epm;
use rs_matter::dm::clusters::decl::power_source as ps;
use rs_matter::dm::clusters::decl::power_topology as pt;
use rs_matter::dm::clusters::decl::temperature_measurement as tm;
use rs_matter::dm::clusters::desc::{self, ClusterHandler as _, DescHandler};
use rs_matter::dm::clusters::identify::{self, IdentifyHandler};
use rs_matter::dm::endpoints::EthSysHandlerBuilder;
use rs_matter::dm::networks::SysNetifs;
use rs_matter::dm::{Async, DataModel, Dataver, DeviceType, Endpoint, EndptId, EpClMatcher, Node};
use rs_matter::{clusters, devices, root_endpoint};

use crate::domain::PvSnapshot;

use super::desc_tags::{
    COMMON_LOCATION_NAMESPACE, COMMON_LOCATION_TAG_INSIDE, POWER_SOURCE_NAMESPACE,
    POWER_SOURCE_TAG_GRID, SemanticTag, SolarPartsMatcher, TaggedDescHandler,
};
use super::eem::EemHandler;
use super::epm::EpmHandler;
use super::power_source::PowerSourceHandler;
use super::power_topology::PowerTopologyHandler;
use super::temperature::TemperatureHandler;

/// Endpoint IDs (Endpoint 0 is the root).
pub const EP_SOLAR_POWER: EndptId = 1;
pub const EP_POWER_SOURCE: EndptId = 2;
pub const EP_ELECTRICAL_SENSOR: EndptId = 3;
pub const EP_TEMPERATURE: EndptId = 4;

// Device types from the Matter 1.5 Device Library (not predefined in
// rs-matter's `dm::devices` at this rev). All are revision 1 in 1.5.1.
const DEV_TYPE_SOLAR_POWER: DeviceType = DeviceType {
    dtype: 0x0017,
    drev: 1,
};
const DEV_TYPE_POWER_SOURCE: DeviceType = DeviceType {
    dtype: 0x0011,
    drev: 1,
};
const DEV_TYPE_ELECTRICAL_SENSOR: DeviceType = DeviceType {
    dtype: 0x0510,
    drev: 1,
};
const DEV_TYPE_TEMPERATURE_SENSOR: DeviceType = DeviceType {
    dtype: 0x0302,
    drev: 1,
};

/// The component endpoints, i.e. the Solar Power endpoint's `PartsList`.
static SOLAR_PARTS: SolarPartsMatcher = SolarPartsMatcher {
    parent: EP_SOLAR_POWER,
    children: &[EP_POWER_SOURCE, EP_ELECTRICAL_SENSOR, EP_TEMPERATURE],
};

/// `EndpointList` for the Power Source cluster: everything this source powers.
pub const POWERED_ENDPOINTS: &[EndptId] = &[
    EP_SOLAR_POWER,
    EP_POWER_SOURCE,
    EP_ELECTRICAL_SENSOR,
    EP_TEMPERATURE,
];

/// Grid tag (Power Source namespace) — mandatory on the Power Source
/// endpoint of a Solar Power device (§14.3.6.4).
static GRID_TAG: [SemanticTag; 1] = [SemanticTag {
    mfg_code: None,
    namespace_id: POWER_SOURCE_NAMESPACE,
    tag: POWER_SOURCE_TAG_GRID,
    label: None,
}];

/// Temperature Sensors "SHALL include Tag(s) … to identify the temperature
/// being measured" (§14.3.6.4). The SMA `device/temperature` reading is the
/// inverter's own internal temperature, so the standard Common Location
/// `Inside` tag ("located inside the equipment") names it precisely — no
/// manufacturer code, and a Label is only required for non-standard namespaces.
/// The label is retained purely for human readability.
static INVERTER_TEMPERATURE_TAG: [SemanticTag; 1] = [SemanticTag {
    mfg_code: None,
    namespace_id: COMMON_LOCATION_NAMESPACE,
    tag: COMMON_LOCATION_TAG_INSIDE,
    label: Some("inverter"),
}];

/// The static node tree. Component endpoints carry Descriptor + Identify plus
/// their functional cluster(s); tagged Descriptors (TagList) ride the Power
/// Source and Temperature endpoints.
pub const NODE: Node<'static> = Node {
    endpoints: &[
        root_endpoint!(eth),
        Endpoint::new(
            EP_SOLAR_POWER,
            devices!(DEV_TYPE_SOLAR_POWER),
            clusters!(desc::DescHandler::CLUSTER, identify::CLUSTER),
        ),
        Endpoint::new(
            EP_POWER_SOURCE,
            devices!(DEV_TYPE_POWER_SOURCE),
            clusters!(
                <TaggedDescHandler as descriptor::ClusterHandler>::CLUSTER,
                identify::CLUSTER,
                <PowerSourceHandler<'static> as ps::ClusterHandler>::CLUSTER
            ),
        ),
        Endpoint::new(
            EP_ELECTRICAL_SENSOR,
            devices!(DEV_TYPE_ELECTRICAL_SENSOR),
            clusters!(
                desc::DescHandler::CLUSTER,
                identify::CLUSTER,
                <PowerTopologyHandler as pt::ClusterHandler>::CLUSTER,
                <EpmHandler<'static> as epm::ClusterHandler>::CLUSTER,
                <EemHandler<'static> as eem::ClusterHandler>::CLUSTER
            ),
        ),
        Endpoint::new(
            EP_TEMPERATURE,
            devices!(DEV_TYPE_TEMPERATURE_SENSOR),
            clusters!(
                <TaggedDescHandler as descriptor::ClusterHandler>::CLUSTER,
                identify::CLUSTER,
                <TemperatureHandler<'static> as tm::ClusterHandler>::CLUSTER
            ),
        ),
    ],
};

/// The functional cluster handlers over the shared snapshot; held by the
/// caller so they outlive the handler chain.
pub struct Handlers<'a> {
    pub power_source: PowerSourceHandler<'a>,
    pub power_topology: PowerTopologyHandler,
    pub epm: EpmHandler<'a>,
    pub eem: EemHandler<'a>,
    pub temperature: TemperatureHandler<'a>,
}

impl<'a> Handlers<'a> {
    pub fn new<R: RngCore>(state: &'a RefCell<PvSnapshot>, rand: &mut R) -> Self {
        Self {
            power_source: PowerSourceHandler::new(
                Dataver::new_rand(rand),
                state,
                POWERED_ENDPOINTS,
            ),
            power_topology: PowerTopologyHandler::new(Dataver::new_rand(rand)),
            epm: EpmHandler::new(Dataver::new_rand(rand), state),
            eem: EemHandler::new(Dataver::new_rand(rand), state),
            temperature: TemperatureHandler::new(Dataver::new_rand(rand), state),
        }
    }
}

/// Builds the data-model handler chain: root system clusters plus, per
/// endpoint, Descriptor + Identify and the functional handler(s). Mirrors
/// [`NODE`].
pub fn dm_handler<'a>(
    mut rand: impl RngCore + Copy,
    handlers: &'a Handlers<'a>,
) -> impl DataModel + 'a {
    (
        NODE,
        EthSysHandlerBuilder::new()
            .netif_diag(&SysNetifs)
            .build(rand)
            // ---- Endpoint 1: Solar Power (composition parent) ----
            .chain(
                EpClMatcher::new(Some(EP_SOLAR_POWER), Some(desc::DescHandler::CLUSTER.id)),
                Async(
                    DescHandler::new_matching(Dataver::new_rand(&mut rand), &SOLAR_PARTS).adapt(),
                ),
            )
            .chain(
                EpClMatcher::new(Some(EP_SOLAR_POWER), Some(identify::CLUSTER.id)),
                Async(IdentifyHandler::new(Dataver::new_rand(&mut rand))),
            )
            // ---- Endpoint 2: Power Source ----
            .chain(
                EpClMatcher::new(Some(EP_POWER_SOURCE), Some(desc::DescHandler::CLUSTER.id)),
                Async(descriptor::HandlerAdaptor(TaggedDescHandler::new(
                    Dataver::new_rand(&mut rand),
                    &GRID_TAG,
                ))),
            )
            .chain(
                EpClMatcher::new(Some(EP_POWER_SOURCE), Some(identify::CLUSTER.id)),
                Async(IdentifyHandler::new(Dataver::new_rand(&mut rand))),
            )
            .chain(
                EpClMatcher::new(Some(EP_POWER_SOURCE), Some(ps::FULL_CLUSTER.id)),
                Async(ps::HandlerAdaptor(&handlers.power_source)),
            )
            // ---- Endpoint 3: Electrical Sensor ----
            .chain(
                EpClMatcher::new(
                    Some(EP_ELECTRICAL_SENSOR),
                    Some(desc::DescHandler::CLUSTER.id),
                ),
                Async(desc::DescHandler::new(Dataver::new_rand(&mut rand)).adapt()),
            )
            .chain(
                EpClMatcher::new(Some(EP_ELECTRICAL_SENSOR), Some(identify::CLUSTER.id)),
                Async(IdentifyHandler::new(Dataver::new_rand(&mut rand))),
            )
            .chain(
                EpClMatcher::new(Some(EP_ELECTRICAL_SENSOR), Some(pt::FULL_CLUSTER.id)),
                Async(pt::HandlerAdaptor(&handlers.power_topology)),
            )
            .chain(
                EpClMatcher::new(Some(EP_ELECTRICAL_SENSOR), Some(epm::FULL_CLUSTER.id)),
                Async(epm::HandlerAdaptor(&handlers.epm)),
            )
            .chain(
                EpClMatcher::new(Some(EP_ELECTRICAL_SENSOR), Some(eem::FULL_CLUSTER.id)),
                Async(eem::HandlerAdaptor(&handlers.eem)),
            )
            // ---- Endpoint 4: Temperature Sensor ----
            .chain(
                EpClMatcher::new(Some(EP_TEMPERATURE), Some(desc::DescHandler::CLUSTER.id)),
                Async(descriptor::HandlerAdaptor(TaggedDescHandler::new(
                    Dataver::new_rand(&mut rand),
                    &INVERTER_TEMPERATURE_TAG,
                ))),
            )
            .chain(
                EpClMatcher::new(Some(EP_TEMPERATURE), Some(identify::CLUSTER.id)),
                Async(IdentifyHandler::new(Dataver::new_rand(&mut rand))),
            )
            .chain(
                EpClMatcher::new(Some(EP_TEMPERATURE), Some(tm::FULL_CLUSTER.id)),
                Async(tm::HandlerAdaptor(&handlers.temperature)),
            ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(id: EndptId) -> &'static Endpoint<'static> {
        NODE.endpoints
            .iter()
            .find(|e| e.id == id)
            .expect("endpoint present")
    }

    fn has_cluster(id: EndptId, cluster_id: u32) -> bool {
        endpoint(id).clusters.iter().any(|c| c.id == cluster_id)
    }

    #[test]
    fn node_is_root_plus_solar_power_composition() {
        let ids: Vec<EndptId> = NODE.endpoints.iter().map(|e| e.id).collect();
        assert_eq!(
            ids,
            vec![
                0,
                EP_SOLAR_POWER,
                EP_POWER_SOURCE,
                EP_ELECTRICAL_SENSOR,
                EP_TEMPERATURE
            ]
        );
        assert!(
            endpoint(EP_SOLAR_POWER)
                .device_types
                .iter()
                .any(|d| d.dtype == 0x0017)
        );
    }

    #[test]
    fn power_source_endpoint_carries_wired_power_source_and_grid_tag() {
        assert!(
            endpoint(EP_POWER_SOURCE)
                .device_types
                .iter()
                .any(|d| d.dtype == 0x0011)
        );
        assert_eq!(ps::FULL_CLUSTER.id, 0x002F);
        assert!(has_cluster(EP_POWER_SOURCE, ps::FULL_CLUSTER.id));

        let cluster = <PowerSourceHandler<'static> as ps::ClusterHandler>::CLUSTER;
        assert_eq!(cluster.feature_map, ps::Feature::WIRED.bits());

        // The Descriptor on this endpoint advertises TagList (Grid tag).
        let desc_cluster = <TaggedDescHandler as descriptor::ClusterHandler>::CLUSTER;
        assert_eq!(
            desc_cluster.feature_map,
            descriptor::Feature::TAG_LIST.bits()
        );
        assert_eq!(GRID_TAG[0].namespace_id, 0x0F);
        assert_eq!(GRID_TAG[0].tag, 0x01);
    }

    #[test]
    fn electrical_sensor_endpoint_carries_topology_epm_eem() {
        assert!(
            endpoint(EP_ELECTRICAL_SENSOR)
                .device_types
                .iter()
                .any(|d| d.dtype == 0x0510)
        );
        assert_eq!(pt::FULL_CLUSTER.id, 0x009C);
        assert_eq!(epm::FULL_CLUSTER.id, 0x0090);
        assert_eq!(eem::FULL_CLUSTER.id, 0x0091);
        assert!(has_cluster(EP_ELECTRICAL_SENSOR, pt::FULL_CLUSTER.id));
        assert!(has_cluster(EP_ELECTRICAL_SENSOR, epm::FULL_CLUSTER.id));
        assert!(has_cluster(EP_ELECTRICAL_SENSOR, eem::FULL_CLUSTER.id));
    }

    #[test]
    fn epm_advertises_ac_with_mandated_voltage_and_current() {
        let cluster = <EpmHandler<'static> as epm::ClusterHandler>::CLUSTER;
        assert_eq!(
            cluster.feature_map,
            epm::Feature::ALTERNATING_CURRENT.bits()
        );
        for attr in [
            epm::AttributeId::PowerMode,
            epm::AttributeId::NumberOfMeasurementTypes,
            epm::AttributeId::Accuracy,
            epm::AttributeId::ActivePower,
            // Solar Power §14.3.6.3 overrides these to mandatory:
            epm::AttributeId::Voltage,
            epm::AttributeId::ActiveCurrent,
            // Optional, but we have the reading:
            epm::AttributeId::Frequency,
        ] {
            assert!(
                cluster.attribute(attr as u32).is_some(),
                "EPM must serve {attr:?}"
            );
        }
        // ReactivePower is honestly absent (no SMA-side reading).
        assert!(
            cluster
                .attribute(epm::AttributeId::ReactivePower as u32)
                .is_none()
        );
    }

    #[test]
    fn eem_advertises_cumulative_exported_energy() {
        let cluster = <EemHandler<'static> as eem::ClusterHandler>::CLUSTER;
        assert_eq!(
            cluster.feature_map,
            eem::Feature::EXPORTED_ENERGY
                .union(eem::Feature::CUMULATIVE_ENERGY)
                .bits()
        );
        assert!(
            cluster
                .attribute(eem::AttributeId::CumulativeEnergyExported as u32)
                .is_some()
        );
        assert!(
            cluster
                .attribute(eem::AttributeId::CumulativeEnergyImported as u32)
                .is_none()
        );
    }

    #[test]
    fn temperature_endpoint_is_tagged() {
        assert!(
            endpoint(EP_TEMPERATURE)
                .device_types
                .iter()
                .any(|d| d.dtype == 0x0302)
        );
        assert!(has_cluster(EP_TEMPERATURE, tm::FULL_CLUSTER.id));
        // Standard Common Location / Inside tag (no manufacturer code needed).
        assert!(INVERTER_TEMPERATURE_TAG[0].mfg_code.is_none());
        assert_eq!(INVERTER_TEMPERATURE_TAG[0].namespace_id, 0x06);
        assert_eq!(INVERTER_TEMPERATURE_TAG[0].tag, 0x02);
    }

    #[test]
    fn solar_parts_matcher_lists_exactly_the_component_endpoints() {
        use rs_matter::dm::clusters::desc::PartsMatcher;

        for ep in [EP_POWER_SOURCE, EP_ELECTRICAL_SENSOR, EP_TEMPERATURE] {
            assert!(SOLAR_PARTS.matches(EP_SOLAR_POWER, ep));
        }
        assert!(!SOLAR_PARTS.matches(EP_SOLAR_POWER, 0));
        assert!(!SOLAR_PARTS.matches(EP_SOLAR_POWER, EP_SOLAR_POWER));
        // Only the Solar Power endpoint uses this matcher's parts view.
        assert!(!SOLAR_PARTS.matches(EP_ELECTRICAL_SENSOR, EP_TEMPERATURE));
    }
}
