use crate::{offload, LayoutOffload};
use common::{LayoutStrategy, Policy};

#[cfg(feature = "ml")]
use crate::ml;
#[cfg(feature = "zns")]
use crate::zns;

pub fn compile(policy: &Policy) -> Box<dyn LayoutOffload + Send + Sync> {
    match &policy.layout.strategy {
        LayoutStrategy::Fixed { .. } => Box::new(offload::CpuFixed::new(policy.clone())),
        LayoutStrategy::AdaptiveEntropy => Box::new(offload::CpuEntropy::new(policy.clone())),
        LayoutStrategy::ZnsGraph {
            zone_size_mib,
            graph_radius,
        } => {
            #[cfg(feature = "zns")]
            {
                return Box::new(zns::ZnsGraphLayout::new(*zone_size_mib, *graph_radius));
            }
            #[cfg(not(feature = "zns"))]
            {
                let _ = (zone_size_mib, graph_radius);
                panic!("ZNS feature disabled");
            }
        }
        LayoutStrategy::Learned { model_path } => {
            #[cfg(feature = "ml")]
            {
                return Box::new(
                    ml::LearnedLayout::load(model_path).expect("failed to load ML layout model"),
                );
            }
            #[cfg(not(feature = "ml"))]
            {
                let _ = model_path;
                panic!("ML feature disabled");
            }
        }
        LayoutStrategy::QuantumReady { merkle_algo } => Box::new(offload::CpuQuantumReady::new(
            policy.clone(),
            merkle_algo.clone(),
        )),
    }
}
