# Phase C: Implementation Details & System Hardening

- **Target Components:** `crates/scaling`, `crates/layout-engine`, `xtask`, `docker`
- **Status:** Ready for Coding
- **Prerequisites:** Phase A (DataMotion) and Phase B (io_uring) are complete; `rdma-sys` available (hardware or SoftRoCE).

## 1. Executive Summary
This specification closes the gap between Phase C architecture and the codebase:
- Security compliance: unblock CI by handling build-only FFI advisories.
- Environment readiness: ensure ML linting works in CI/Docker by bundling LibTorch.
- RDMA transport: implement QP state machine, OOB handshake, and peer wiring.

## 2. Workstream 1: Dependency Security (Immediate)
- Bindgen (via `rdma-sys`) pulls `ansi_term`/`atty`; these are build-time only.
- Update `deny.toml` advisories ignore list to include:
  - `RUSTSEC-2021-0145` (atty, unmaintained)
  - `RUSTSEC-2021-0139` (ansi_term, unmaintained)
- Rationale: build-time codegen only; no runtime surface.

## 3. Workstream 2: Environment Hardening (LibTorch)
- Docker builder installs LibTorch CPU to allow `layout-engine` lint/build:
  ```dockerfile
  ENV LIBTORCH_VERSION=2.4.0
  RUN curl -L https://download.pytorch.org/libtorch/cpu/libtorch-cxx11-abi-shared-with-deps-${LIBTORCH_VERSION}%2Bcpu.zip -o libtorch.zip \
      && unzip libtorch.zip -d /usr/local/ \
      && rm libtorch.zip
  ENV LIBTORCH=/usr/local/libtorch
  ENV LD_LIBRARY_PATH=$LIBTORCH/lib:$LD_LIBRARY_PATH
  ENV LIBTORCH_USE_PYTORCH=1
  ```
- `xtask` guardrails: warn when `LIBTORCH`/`LIBTORCH_USE_PYTORCH` is missing; allow opt-out for local runs, but enforce in CI.

## 4. Workstream 3: RDMA Transport Implementation
- Location: `crates/scaling/src/transport/rdma.rs`.
- Handshake DTO exchanged over OOB TCP/io_uring before verbs use:
  ```rust
  #[derive(Serialize, Deserialize, Debug)]
  pub struct RdmaHandshake {
      pub lid: u16,
      pub qpn: u32,
      pub psn: u32,
      pub rkey: u32,
      pub vaddr: u64,
  }
  ```
- Transport struct (hardware + peers):
  ```rust
  pub struct RdmaTransport {
      ctx: NonNull<ibv_context>,
      pd: NonNull<ibv_pd>,
      cq: NonNull<ibv_cq>,
      local_lid: u16,
      local_qpn: u32,
      peers: RwLock<HashMap<NodeId, RdmaPeer>>,
  }

  struct RdmaPeer {
      qp: NonNull<ibv_qp>,
      rkey: u32,
      raddr: u64,
  }
  ```
- QP state machine (RESET→INIT→RTR→RTS):
  - INIT: `qp_state=INIT`, `port_num=1`, access flags `LOCAL_WRITE|REMOTE_WRITE|REMOTE_READ`.
  - RTR: set `dest_qp_num`, `rq_psn`, `path_mtu`, `ah_attr` from `{lid,gid}`, `max_dest_rd_atomic=1`, `min_rnr_timer=12`.
  - RTS: set `sq_psn`, `timeout=14`, `retry_cnt=7`, `rnr_retry=7`, `max_rd_atomic=1`.
  - Use `ibv_modify_qp` masks: `STATE`, `PKEY_INDEX`, `PORT`, `ACCESS_FLAGS`, `AV`, `PATH_MTU`, `DEST_QPN`, `RQ_PSN`, `MAX_DEST_RD_ATOMIC`, `MIN_RNR_TIMER`, `TIMEOUT`, `RETRY_CNT`, `RNR_RETRY`, `SQ_PSN`, `MAX_RD_ATOMIC`.

## 5. Workstream 4: Integration (ScalingAgent)
- Agent performs OOB handshake before RDMA send:
  1. Establish TCP/io_uring connection.
  2. Build local `RdmaHandshake`.
  3. Exchange handshake with peer.
  4. Call `rdma.connect_peer(target, remote_info)` to create QP and transition to RTS.
- Data path: allocate registered buffer, fill from NVRAM, `ibv_post_send` on connected QP, await CQ completion.

## 6. Verification
- Security: `cargo deny check advisories` passes with documented ignores.
- Lint: `cargo clippy --workspace` succeeds in Docker (LibTorch present).
- Build: `cargo build --features rdma`.
- Simulate: `scripts/setup_softroce.sh` then `cargo test -p scaling --features rdma --test rdma_integration` (loopback/SoftRoCE).
