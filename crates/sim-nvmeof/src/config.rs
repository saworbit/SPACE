//! Configuration for the NVMe-oF simulation target.
//! Maintained separately so both SPDK and native paths share the same settings.

#[derive(Debug, Clone)]
pub struct NvmeofSimConfig {
    pub node_id: String,
    pub backing_path: String,
    pub listen_addr: String,
    pub listen_port: u16,
    pub subsystem_nqn: String,
}

impl Default for NvmeofSimConfig {
    fn default() -> Self {
        Self {
            node_id: "sim-node1".to_string(),
            backing_path: "sim_nvmeof.img".to_string(),
            listen_addr: "0.0.0.0".to_string(),
            listen_port: 4420,
            subsystem_nqn: "nqn.2024-01.io.space:sim".to_string(),
        }
    }
}
