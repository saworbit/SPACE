#[cfg(feature = "zns")]
use crate::{LayoutOffload, ZonePlan};
#[cfg(feature = "zns")]
use anyhow::Result;
#[cfg(feature = "zns")]
use common::{CapsuleId, Policy};

#[cfg(feature = "zns")]
pub struct ZnsGraphLayout {
    _zone_size: u64,
    _graph_radius: u32,
}

#[cfg(feature = "zns")]
impl ZnsGraphLayout {
    pub fn new(zone_size_mib: u32, graph_radius: u32) -> Self {
        Self {
            _zone_size: zone_size_mib as u64 * 1024 * 1024,
            _graph_radius: graph_radius,
        }
    }
}

#[cfg(feature = "zns")]
impl LayoutOffload for ZnsGraphLayout {
    fn synthesize(
        &self,
        _capsules: &[CapsuleId],
        _data_slices: &[&[u8]],
        _policy: &Policy,
    ) -> Result<ZonePlan> {
        todo!("ZNS implementation")
    }
}
