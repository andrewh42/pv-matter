//! Power Topology (0x009C) on the Electrical Sensor endpoint: feature
//! `NODE` — this sensor measures the power for the entire node at the AC
//! connection point. NODE has no non-global attributes, so the handler is
//! only cluster metadata + a dataver.

use rs_matter::dm::clusters::decl::power_topology as pt;
use rs_matter::dm::{Cluster, Dataver};
use rs_matter::with;

pub struct PowerTopologyHandler {
    dataver: Dataver,
}

impl PowerTopologyHandler {
    pub fn new(dataver: Dataver) -> Self {
        Self { dataver }
    }
}

impl pt::ClusterHandler for PowerTopologyHandler {
    const CLUSTER: Cluster<'static> = pt::FULL_CLUSTER
        .with_features(pt::Feature::NODE_TOPOLOGY.bits())
        .with_attrs(with!(required));

    fn dataver(&self) -> u32 {
        self.dataver.get()
    }

    fn dataver_changed(&self) {
        self.dataver.changed();
    }
}
