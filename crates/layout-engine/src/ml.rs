#[cfg(feature = "ml")]
use anyhow::Result;
#[cfg(feature = "ml")]
use common::{CapsuleId, Policy};
#[cfg(feature = "ml")]
use tch;

#[cfg(feature = "ml")]
use crate::{offload, LayoutOffload, ZonePlan};

#[cfg(feature = "ml")]
pub struct LearnedLayout {
    model: tch::CModule,
}

#[cfg(feature = "ml")]
impl LearnedLayout {
    pub fn load(path: &str) -> Result<Self> {
        let model = tch::CModule::load(path)?;
        Ok(Self { model })
    }
}

#[cfg(feature = "ml")]
impl LayoutOffload for LearnedLayout {
    fn synthesize(
        &self,
        capsules: &[CapsuleId],
        data_slices: &[&[u8]],
        policy: &Policy,
    ) -> Result<ZonePlan> {
        let plan =
            offload::CpuFixed::new(policy.clone()).synthesize(capsules, data_slices, policy)?;
        Ok(plan)
    }
}
