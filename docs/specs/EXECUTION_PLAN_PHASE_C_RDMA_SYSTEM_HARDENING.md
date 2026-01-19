# Execution Plan: Phase C RDMA & System Hardening

- **Target Components:** `crates/scaling`, `crates/layout-engine`, `xtask`
- **Status:** Approved for Immediate Execution
- **Prerequisites:** Phase A (DataMotion) & Phase B (io_uring) complete

## 1. Executive Summary
- Transition from simulated to hardware-backed performance by wiring InfiniBand verbs for zero-copy replication.
- Harden the supply chain by addressing rdma-sys/bindgen advisory noise.
- Restore full-workspace static analysis when LibTorch is absent.

## 2. Workstream 1: RDMA Transport Implementation
- Implement QP state transitions (RESET→INIT→RTR→RTS) using the Phase B TCP/io_uring path as OOB control to exchange `{QPN, LID, PSN, GID}`.
- Add handshake DTO:
  ```rust
  #[derive(Serialize, Deserialize)]
  struct RdmaHandshake { qpn: u32, psn: u32, lid: u16, gid: [u8; 16] }
  ```
- `RdmaTransport::connect_qp` (marked `unsafe`):
  - **Safety**: Caller must ensure exclusive access to `qp` during state transitions.
    The underlying `ibv_modify_qp` C-function is not thread-safe for concurrent QP access.
  - INIT: set `qp_state=INIT`, `port_num=1`, `qp_access_flags=IBV_ACCESS_REMOTE_WRITE|LOCAL_WRITE`.
  - RTR: set `dest_qp_num=remote.qpn`, `rq_psn=remote.psn`, `path_mtu=4096`, attach AH from `{lid,gid}`.
  - RTS: set `sq_psn=local_psn`, `timeout=14`, `retry_cnt=7`, `rnr_retry=7`.
- Maintain CQ polling actor; replace io_uring fallback once QP negotiation is stable.

## 3. Workstream 2: Dependency Hygiene
- Bindgen (rdma-sys build-time) pulls `ansi_term`/`atty`; add explicit allowlist entries in `deny.toml` and `.cargo/audit.toml` for these build-only advisories.
- Contingency: if upstream does not refresh bindgen, vendor `rdma-sys` and bump bindgen (>=0.69) to shed deprecated deps.

## 4. Workstream 3: Code Integrity (ML & Clippy)
- `xtask` must warn (and fail in CI) when `LIBTORCH`/`LIBTORCH_USE_PYTORCH` is missing to prevent skipping `layout-engine` lint/build.
- Builder image installs LibTorch CPU (`libtorch-cxx11-abi-shared-with-deps-2.4.0+cpu.zip`) and exports `LIBTORCH`, `LD_LIBRARY_PATH` to unblock clippy/check in CI.

## 5. Next Steps
- Wire OOB handshake into ScalingAgent, then replace the RDMA fallback send path with real `ibv_post_send`.
- Add loopback verbs test using SoftRoCE (RXE) in CI.
- Revisit rdma-sys when upstream bindgen refresh lands to drop the advisory ignores.
