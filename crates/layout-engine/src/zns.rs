#[cfg(feature = "zns")]
use crate::{LayoutOffload, ZonePlan};
#[cfg(feature = "zns")]
use anyhow::Result;
#[cfg(feature = "zns")]
use common::{CapsuleId, Policy};

#[cfg(feature = "zns")]
pub struct ZnsGraphLayout {
    zone_size: u64,
    graph_radius: u32,
}

#[cfg(feature = "zns")]
impl ZnsGraphLayout {
    pub fn new(zone_size_mib: u32, graph_radius: u32) -> Self {
        Self {
            zone_size: zone_size_mib as u64 * 1024 * 1024,
            graph_radius,
        }
    }
}

#[cfg(feature = "zns")]
impl LayoutOffload for ZnsGraphLayout {
    fn synthesize(
        &self,
        capsules: &[CapsuleId],
        data_slices: &[&[u8]],
        policy: &Policy,
    ) -> Result<ZonePlan> {
        todo!("ZNS implementation")
    }
}
