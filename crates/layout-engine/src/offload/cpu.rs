use anyhow::Result;
use blake3;
use common::{CapsuleId, ContentHash, MerkleAlgo, Policy};

use crate::{LayoutOffload, SegmentRef, Zone, ZonePlan};

#[cfg(feature = "pq")]
use sha3::{Digest, Sha3_256};

fn hash_chunk(data: &[u8]) -> ContentHash {
    ContentHash::from_bytes(blake3::hash(data).as_bytes())
}

fn capsule_id_for(capsules: &[CapsuleId]) -> CapsuleId {
    capsules.get(0).copied().unwrap_or_default()
}

pub struct CpuFixed {
    policy: Policy,
}

impl CpuFixed {
    pub fn new(policy: Policy) -> Self {
        Self { policy }
    }

    fn build_plan(&self, capsules: &[CapsuleId], data_slices: &[&[u8]]) -> ZonePlan {
        let segment_size = self.policy.layout.strategy.default_segment_size();
        let mut zones = Vec::new();
        let mut zone_id = 0u64;
        let mut current_zone = Zone {
            id: zone_id,
            iv_seed: zone_id,
            segments: Vec::new(),
        };
        let mut zone_usage = 0usize;
        let mut cursor = 0usize;
        let capsule = capsule_id_for(capsules);

        for slice in data_slices {
            let mut start = 0usize;
            while start < slice.len() {
                let take = (slice.len() - start).min(segment_size);
                let chunk = &slice[start..start + take];
                let segment = SegmentRef {
                    capsule_id: capsule,
                    offset: (cursor + start) as u64,
                    length: take as u64,
                    compressed_hash: hash_chunk(chunk),
                };
                current_zone.segments.push(segment);
                start += take;
                zone_usage += take;

                if zone_usage >= self.policy.layout.strategy.default_segment_size()
                    && !current_zone.segments.is_empty()
                {
                    zones.push(current_zone);
                    zone_id += 1;
                    current_zone = Zone {
                        id: zone_id,
                        iv_seed: zone_id,
                        segments: Vec::new(),
                    };
                    zone_usage = 0;
                }
            }
            cursor += slice.len();
        }

        if !current_zone.segments.is_empty() {
            zones.push(current_zone);
        }

        ZonePlan {
            zones,
            merkle_root: None,
        }
    }
}

impl LayoutOffload for CpuFixed {
    fn synthesize(
        &self,
        capsules: &[CapsuleId],
        data_slices: &[&[u8]],
        _policy: &Policy,
    ) -> Result<ZonePlan> {
        Ok(self.build_plan(capsules, data_slices))
    }
}

pub struct CpuEntropy {
    policy: Policy,
}

impl CpuEntropy {
    pub fn new(policy: Policy) -> Self {
        Self { policy }
    }
}

impl LayoutOffload for CpuEntropy {
    fn synthesize(
        &self,
        capsules: &[CapsuleId],
        data_slices: &[&[u8]],
        policy: &Policy,
    ) -> Result<ZonePlan> {
        let fallback = CpuFixed::new(self.policy.clone());
        fallback.synthesize(capsules, data_slices, policy)
    }
}

pub struct CpuQuantumReady {
    policy: Policy,
    merkle_algo: MerkleAlgo,
}

impl CpuQuantumReady {
    pub fn new(policy: Policy, merkle_algo: MerkleAlgo) -> Self {
        Self {
            policy,
            merkle_algo,
        }
    }

    fn compute_merkle_root(&self, data_slices: &[&[u8]]) -> ContentHash {
        match self.merkle_algo {
            MerkleAlgo::Blake3 => {
                let mut hasher = blake3::Hasher::new();
                for slice in data_slices {
                    hasher.update(slice);
                }
                ContentHash::from_bytes(hasher.finalize().as_bytes())
            }
            MerkleAlgo::SphincsPlus => {
                #[cfg(feature = "pq")]
                {
                    let mut hasher = Sha3_256::new();
                    for slice in data_slices {
                        hasher.update(slice);
                    }
                    ContentHash::from_bytes(hasher.finalize().as_slice())
                }
                #[cfg(not(feature = "pq"))]
                {
                    let mut concat = Vec::new();
                    for slice in data_slices {
                        concat.extend_from_slice(slice);
                    }
                    hash_chunk(&concat)
                }
            }
        }
    }
}

impl LayoutOffload for CpuQuantumReady {
    fn synthesize(
        &self,
        capsules: &[CapsuleId],
        data_slices: &[&[u8]],
        policy: &Policy,
    ) -> Result<ZonePlan> {
        let mut plan =
            CpuFixed::new(self.policy.clone()).synthesize(capsules, data_slices, policy)?;
        plan.merkle_root = Some(self.compute_merkle_root(data_slices));
        Ok(plan)
    }
}
