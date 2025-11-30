//! Native NVMe-oF TCP simulation target implemented in Rust.
//!
//! This target implements enough of the NVMe/TCP binding to let `nvme discover`
//! and `nvme connect` talk to a simulated controller without SPDK or hugepages.
//! The implementation is intentionally small and synchronous to keep the sim
//! footprint minimal inside CI and container environments.

use crate::config::NvmeofSimConfig;
use anyhow::{anyhow, Context, Result};
use byteorder::{ByteOrder, LittleEndian};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::thread;
use tracing::{debug, error, info, warn};

// NVMe/TCP protocol constants
const NVME_TCP_HDR_SIZE: usize = 8;
const NVME_TCP_IC_PDU_SIZE: usize = 128;
const NVME_CMD_SIZE: usize = 64;
const NVME_CQE_SIZE: usize = 16;
const NVME_TCP_DATA_PDU_SIZE: usize = 24;
const BLOCK_SIZE: u64 = 4096;

// PDU types
const PDU_TYPE_ICREQ: u8 = 0x00;
const PDU_TYPE_ICRESP: u8 = 0x01;
const PDU_TYPE_CAPSULE_CMD: u8 = 0x04;
const PDU_TYPE_CAPSULE_RESP: u8 = 0x05;
const PDU_TYPE_H2C_DATA: u8 = 0x06;
const PDU_TYPE_C2H_DATA: u8 = 0x07;

// NVMe opcodes
const NVME_OP_FABRICS: u8 = 0x7F;
const NVME_OP_ADMIN_GET_LOG_PAGE: u8 = 0x02;
const NVME_OP_ADMIN_IDENTIFY: u8 = 0x06;
const NVME_OP_READ: u8 = 0x02;
const NVME_OP_WRITE: u8 = 0x01;
const NVME_OP_KEEP_ALIVE: u8 = 0x18;

// Fabrics command types
const NVME_FABRICS_CMD_CONNECT: u8 = 0x01;

// Discovery constants
const NVME_DISCOVERY_NQN: &str = "nqn.2014-08.org.nvmexpress.discovery";

/// Start the Native Rust NVMe-oF TCP target (fallback mode).
pub fn start_native_tcp_target(config: NvmeofSimConfig) -> Result<()> {
    info!(?config, "Starting Native NVMe-oF TCP Simulation Target");

    ensure_backing_file(&config.backing_path)?;

    let addr = format!("{}:{}", config.listen_addr, config.listen_port);
    let listener = TcpListener::bind(&addr).context("Failed to bind NVMe-oF TCP listener")?;

    info!(%addr, "NVMe-oF TCP target listening");

    let shared_config = Arc::new(config);
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let cfg = shared_config.clone();
                let backing = cfg.backing_path.clone();
                thread::spawn(move || {
                    if let Err(err) = handle_connection(stream, cfg, backing) {
                        warn!("Connection handling failed: {err:?}");
                    }
                });
            }
            Err(err) => error!("TCP accept error: {err:?}"),
        }
    }

    Ok(())
}

fn ensure_backing_file(path: &str) -> Result<()> {
    let path_obj = Path::new(path);
    if let Some(parent) = path_obj.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    if !path_obj.exists() {
        info!("Creating 100MB sparse backing file at {path}");
        let file = File::create(path_obj)?;
        file.set_len(100 * 1024 * 1024)?;
    }
    Ok(())
}

// ============================================================================//
// Connection lifecycle                                                         //
// ============================================================================//

#[derive(Debug)]
struct NvmeSession {
    config: Arc<NvmeofSimConfig>,
    file: File,
    controller_id: u16,
    queue_id: u16,
    discovery_mode: bool,
    connected_nqn: String,
    phase: u16,
}

#[derive(Debug, Clone, Copy)]
struct NvmeTcpHeader {
    pdu_type: u8,
    _flags: u8,
    hlen: u8,
    _pdo: u8,
    plen: u32,
}

fn handle_connection(
    mut stream: TcpStream,
    config: Arc<NvmeofSimConfig>,
    backing_path: String,
) -> Result<()> {
    debug!("New NVMe/TCP connection from {:?}", stream.peer_addr()?);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&backing_path)
        .context("Failed to open backing file")?;

    let mut session = NvmeSession {
        config,
        file,
        controller_id: 1,
        queue_id: 0,
        discovery_mode: false,
        connected_nqn: String::new(),
        phase: 1, // first completions use phase 1
    };

    handle_ic_handshake(&mut stream)?;

    loop {
        let header = match read_header(&mut stream) {
            Ok(h) => h,
            Err(err) => {
                debug!("Connection closed or invalid header: {err:?}");
                break;
            }
        };

        if usize::from(header.hlen) < NVME_TCP_HDR_SIZE {
            return Err(anyhow!("Invalid PDU header length {}", header.hlen));
        }

        let header_payload_len = header.hlen as usize - NVME_TCP_HDR_SIZE;
        let mut header_payload = vec![0u8; header_payload_len];
        stream.read_exact(&mut header_payload)?;

        let total_len = header.plen as usize;
        if total_len < header.hlen as usize {
            return Err(anyhow!(
                "Invalid PDU length: plen {} smaller than hlen {}",
                header.plen,
                header.hlen
            ));
        }

        let data_len = total_len - header.hlen as usize;
        let mut data = vec![0u8; data_len];
        if data_len > 0 {
            stream.read_exact(&mut data)?;
        }

        match header.pdu_type {
            PDU_TYPE_CAPSULE_CMD => {
                handle_capsule_cmd(&mut stream, &mut session, &header_payload, &data)?
            }
            PDU_TYPE_H2C_DATA => debug!("Ignoring unexpected H2C Data PDU (len={data_len})"),
            other => warn!("Unhandled PDU type 0x{other:02x}"),
        }
    }

    Ok(())
}

fn handle_ic_handshake(stream: &mut TcpStream) -> Result<()> {
    let mut icreq = [0u8; NVME_TCP_IC_PDU_SIZE];
    stream.read_exact(&mut icreq)?;

    if icreq[0] != PDU_TYPE_ICREQ {
        return Err(anyhow!("Expected ICReq PDU, got type {}", icreq[0]));
    }

    let hlen = icreq[2];
    let plen = LittleEndian::read_u32(&icreq[4..8]);
    if hlen as u32 != plen || hlen as usize != NVME_TCP_IC_PDU_SIZE {
        warn!("ICReq length mismatch (hlen={hlen}, plen={plen})");
    }

    let mut icresp = [0u8; NVME_TCP_IC_PDU_SIZE];
    icresp[0] = PDU_TYPE_ICRESP;
    icresp[1] = 0;
    icresp[2] = NVME_TCP_IC_PDU_SIZE as u8;
    icresp[3] = 0; // no in-capsule data
    LittleEndian::write_u32(&mut icresp[4..8], NVME_TCP_IC_PDU_SIZE as u32);
    LittleEndian::write_u16(&mut icresp[8..10], 0); // PFV 1.0
    icresp[10] = 0; // CPDA
    icresp[11] = 0; // digest disabled
    LittleEndian::write_u32(&mut icresp[12..16], 1024 * 1024); // Max data

    stream.write_all(&icresp)?;
    debug!("ICReq/ICResp handshake completed");
    Ok(())
}

fn read_header(stream: &mut TcpStream) -> Result<NvmeTcpHeader> {
    let mut buf = [0u8; NVME_TCP_HDR_SIZE];
    stream.read_exact(&mut buf)?;
    Ok(NvmeTcpHeader {
        pdu_type: buf[0],
        _flags: buf[1],
        hlen: buf[2],
        _pdo: buf[3],
        plen: LittleEndian::read_u32(&buf[4..8]),
    })
}

// ============================================================================//
// Command handling                                                             //
// ============================================================================//

fn handle_capsule_cmd(
    stream: &mut TcpStream,
    session: &mut NvmeSession,
    header_payload: &[u8],
    data: &[u8],
) -> Result<()> {
    if header_payload.len() < NVME_CMD_SIZE {
        return Err(anyhow!(
            "CapsuleCmd header too small: {} < {}",
            header_payload.len(),
            NVME_CMD_SIZE
        ));
    }

    let sqe = &header_payload[0..NVME_CMD_SIZE];
    let opcode = sqe[0];
    let cid = LittleEndian::read_u16(&sqe[2..4]);
    let is_admin_queue = session.queue_id == 0;

    debug!("Received opcode=0x{opcode:02x}, cid={cid}");

    match opcode {
        NVME_OP_FABRICS => handle_fabrics_cmd(stream, session, sqe, data, cid),
        op if is_admin_queue && op == NVME_OP_ADMIN_GET_LOG_PAGE => {
            handle_get_log_page(stream, session, sqe, cid)
        }
        op if is_admin_queue && op == NVME_OP_ADMIN_IDENTIFY => {
            handle_identify(stream, session, sqe, cid)
        }
        NVME_OP_KEEP_ALIVE => send_success_response(stream, session, cid, 0),
        NVME_OP_READ => handle_io_read(stream, session, sqe, cid),
        NVME_OP_WRITE => handle_io_write(stream, session, sqe, data, cid),
        other => {
            warn!("Unhandled opcode 0x{other:02x}, returning success placeholder");
            send_success_response(stream, session, cid, 0)
        }
    }
}

fn handle_fabrics_cmd(
    stream: &mut TcpStream,
    session: &mut NvmeSession,
    sqe: &[u8],
    data: &[u8],
    cid: u16,
) -> Result<()> {
    let fctype = sqe[4];
    match fctype {
        NVME_FABRICS_CMD_CONNECT => {
            let qid = LittleEndian::read_u16(&sqe[44..46]);
            session.queue_id = qid;

            let connect_nqn =
                parse_connect_nqn(data).unwrap_or_else(|| NVME_DISCOVERY_NQN.to_string());
            session.discovery_mode = connect_nqn == NVME_DISCOVERY_NQN;
            session.connected_nqn = connect_nqn.clone();

            debug!(
                qid,
                discovery = session.discovery_mode,
                ?connect_nqn,
                "Connect request handled"
            );

            send_success_response(stream, session, cid, session.controller_id as u32)
        }
        _ => {
            warn!("Unsupported fabrics command type 0x{fctype:02x}");
            send_success_response(stream, session, cid, 0)
        }
    }
}

fn handle_get_log_page(
    stream: &mut TcpStream,
    session: &mut NvmeSession,
    sqe: &[u8],
    cid: u16,
) -> Result<()> {
    let cdw10 = LittleEndian::read_u32(&sqe[40..44]);
    let cdw11 = LittleEndian::read_u32(&sqe[44..48]);
    let log_id = (cdw10 & 0xFF) as u8;
    let numdl = (cdw10 >> 16) & 0xFFFF;
    let numdu = cdw11 & 0xFFFF;
    let numd = (numdu << 16) | numdl;
    let requested_len = if numd > 0 { (numd + 1) as usize * 4 } else { 0 };

    if log_id == 0x70 {
        let log = build_discovery_log(&session.config);
        let log_len = log.len();
        let mut payload = if requested_len == 0 {
            log.clone()
        } else {
            let mut buf = vec![0u8; requested_len];
            let copy_len = buf.len().min(log.len());
            buf[..copy_len].copy_from_slice(&log[..copy_len]);
            buf
        };

        // If the caller requested more than we built, pad with zeros.
        if payload.len() < log_len {
            payload = log;
        }

        send_c2h_data(stream, cid, &payload)?;
        send_success_response(stream, session, cid, 0)
    } else {
        warn!("Get Log Page for unsupported log_id=0x{log_id:02x}");
        send_success_response(stream, session, cid, 0)
    }
}

fn handle_identify(
    stream: &mut TcpStream,
    session: &mut NvmeSession,
    sqe: &[u8],
    cid: u16,
) -> Result<()> {
    let nsid = LittleEndian::read_u32(&sqe[4..8]);
    let cns = sqe[41]; // CNS is bits 7:0 of CDW10
    debug!(nsid, cns, "Identify command");

    let data = if cns == 0x01 || nsid == 0 {
        build_identify_controller(session)
    } else {
        build_identify_namespace(session, nsid)
    };

    send_c2h_data(stream, cid, &data)?;
    send_success_response(stream, session, cid, 0)
}

fn handle_io_read(
    stream: &mut TcpStream,
    session: &mut NvmeSession,
    sqe: &[u8],
    cid: u16,
) -> Result<()> {
    let slba = LittleEndian::read_u64(&sqe[40..48]);
    let nlb = (LittleEndian::read_u16(&sqe[48..50]) as u64) + 1;
    let offset = slba * BLOCK_SIZE;
    let len = nlb * BLOCK_SIZE;

    let mut buf = vec![0u8; len as usize];
    session.file.seek(SeekFrom::Start(offset))?;
    session.file.read_exact(&mut buf)?;

    send_c2h_data(stream, cid, &buf)?;
    send_success_response(stream, session, cid, 0)
}

fn handle_io_write(
    stream: &mut TcpStream,
    session: &mut NvmeSession,
    sqe: &[u8],
    data: &[u8],
    cid: u16,
) -> Result<()> {
    let slba = LittleEndian::read_u64(&sqe[40..48]);
    let nlb = (LittleEndian::read_u16(&sqe[48..50]) as u64) + 1;
    let offset = slba * BLOCK_SIZE;
    let len = nlb * BLOCK_SIZE;

    if data.len() < len as usize {
        warn!(
            expected = len,
            actual = data.len(),
            "Write data shorter than expected; dropping request"
        );
        return send_success_response(stream, session, cid, 0);
    }

    session.file.seek(SeekFrom::Start(offset))?;
    session.file.write_all(&data[..len as usize])?;
    session.file.flush()?;

    send_success_response(stream, session, cid, 0)
}

// ============================================================================//
// Response helpers                                                             //
// ============================================================================//

fn send_success_response(
    stream: &mut TcpStream,
    session: &mut NvmeSession,
    cid: u16,
    result: u32,
) -> Result<()> {
    send_response(stream, session, cid, 0, result)
}

fn send_response(
    stream: &mut TcpStream,
    session: &mut NvmeSession,
    cid: u16,
    status: u16,
    result: u32,
) -> Result<()> {
    let mut header = [0u8; NVME_TCP_HDR_SIZE];
    let mut cqe = [0u8; NVME_CQE_SIZE];

    header[0] = PDU_TYPE_CAPSULE_RESP;
    header[1] = 0;
    header[2] = (NVME_TCP_HDR_SIZE + NVME_CQE_SIZE) as u8;
    header[3] = 0;
    LittleEndian::write_u32(
        &mut header[4..8],
        (NVME_TCP_HDR_SIZE + NVME_CQE_SIZE) as u32,
    );

    LittleEndian::write_u32(&mut cqe[0..4], result);
    LittleEndian::write_u16(&mut cqe[4..6], 0); // SQ head
    LittleEndian::write_u16(&mut cqe[6..8], session.queue_id);
    LittleEndian::write_u16(&mut cqe[12..14], cid);
    let status_field = (status << 1) | (session.phase & 0x1);
    LittleEndian::write_u16(&mut cqe[14..16], status_field);

    stream.write_all(&header)?;
    stream.write_all(&cqe)?;
    Ok(())
}

fn send_c2h_data(stream: &mut TcpStream, cid: u16, data: &[u8]) -> Result<()> {
    let mut header = [0u8; NVME_TCP_HDR_SIZE];
    header[0] = PDU_TYPE_C2H_DATA;
    header[1] = 0x0C; // DATA_LAST | DATA_SUCCESS
    header[2] = NVME_TCP_DATA_PDU_SIZE as u8;
    header[3] = NVME_TCP_DATA_PDU_SIZE as u8; // data starts immediately after header
    LittleEndian::write_u32(
        &mut header[4..8],
        (NVME_TCP_DATA_PDU_SIZE + data.len()) as u32,
    );

    let mut data_hdr = [0u8; NVME_TCP_DATA_PDU_SIZE - NVME_TCP_HDR_SIZE];
    LittleEndian::write_u16(&mut data_hdr[0..2], cid);
    // ttag at [2..4] stays zero
    LittleEndian::write_u32(&mut data_hdr[4..8], 0); // data offset
    LittleEndian::write_u32(&mut data_hdr[8..12], data.len() as u32);

    stream.write_all(&header)?;
    stream.write_all(&data_hdr)?;
    stream.write_all(data)?;
    Ok(())
}

// ============================================================================//
// Data builders                                                                //
// ============================================================================//

fn parse_connect_nqn(data: &[u8]) -> Option<String> {
    if data.len() < 1024 {
        return None;
    }
    let subnqn = &data[256..512];
    let len = subnqn.iter().position(|b| *b == 0).unwrap_or(subnqn.len());
    let nqn = String::from_utf8_lossy(&subnqn[..len]).trim().to_string();
    if nqn.is_empty() {
        None
    } else {
        Some(nqn)
    }
}

fn build_discovery_log(config: &NvmeofSimConfig) -> Vec<u8> {
    let mut log = vec![0u8; 1024 + 1024];
    LittleEndian::write_u64(&mut log[0..8], 1); // genctr
    LittleEndian::write_u64(&mut log[8..16], 1); // numrec
    LittleEndian::write_u16(&mut log[16..18], 0); // recfmt

    let entry_offset = 1024;
    let entry = &mut log[entry_offset..];
    entry[0] = 0x03; // trtype = TCP
    entry[1] = 0x01; // adrfam = IPv4
    entry[2] = 0x01; // subtype = NVM subsystem
    entry[3] = 0x00; // treq = not specified
    LittleEndian::write_u16(&mut entry[4..6], 0); // portid
    LittleEndian::write_u16(&mut entry[6..8], 0xffff); // cntlid wildcard
    LittleEndian::write_u16(&mut entry[8..10], 32); // asqsz

    let port_str = format!("{}", config.listen_port);
    let port_bytes = port_str.as_bytes();
    let port_len = port_bytes.len().min(32);
    entry[32..32 + port_len].copy_from_slice(&port_bytes[..port_len]);

    let addr_bytes = config.listen_addr.as_bytes();
    let addr_len = addr_bytes.len().min(256);
    entry[512..512 + addr_len].copy_from_slice(&addr_bytes[..addr_len]); // traddr

    let nqn_bytes = config.subsystem_nqn.as_bytes();
    let nqn_len = nqn_bytes.len().min(256);
    entry[256..256 + nqn_len].copy_from_slice(&nqn_bytes[..nqn_len]); // subnqn

    entry[768] = 0; // tsas.tcp.sectype = none

    log
}

fn build_identify_controller(session: &NvmeSession) -> Vec<u8> {
    let mut data = vec![0u8; 4096];
    LittleEndian::write_u16(&mut data[0..2], 0xFFFF); // VID
    LittleEndian::write_u16(&mut data[2..4], 0xFFFF); // SSVID

    write_padded_ascii(&mut data[4..24], "SIMNVME-CTRL");
    write_padded_ascii(&mut data[24..64], "Space NVMe/TCP Sim");
    write_padded_ascii(&mut data[64..72], "0.1.0");

    data[72] = 0; // RAB
    data[73..76].copy_from_slice(&[0xAA, 0xBB, 0xCC]); // IEEE OUI placeholder
    data[76] = 0; // CMIC
    data[77] = 4; // MDTS (max data transfer size exponent)

    LittleEndian::write_u16(&mut data[78..80], session.controller_id);
    LittleEndian::write_u32(&mut data[80..84], 0x00010400); // Version 1.4

    data[111] = 1; // CNTRLTYPE = I/O Controller

    data
}

fn build_identify_namespace(session: &NvmeSession, _nsid: u32) -> Vec<u8> {
    let mut data = vec![0u8; 4096];
    let file_len = session.file.metadata().map(|m| m.len()).unwrap_or(0);
    let nsze = file_len / BLOCK_SIZE;

    LittleEndian::write_u64(&mut data[0..8], nsze);
    LittleEndian::write_u64(&mut data[8..16], nsze);
    LittleEndian::write_u64(&mut data[16..24], nsze);

    data[24] = 0; // nsfeat
    data[25] = 0; // nlbaf (one format)
    data[26] = 0; // flbas selects LBAF 0
    data[27] = 0; // mc
    data[28] = 0; // dpc
    data[29] = 0; // dps

    // LBAF 0 definition starts at offset 128 (16 bytes each)
    let lbaf0 = 128;
    LittleEndian::write_u16(&mut data[lbaf0..lbaf0 + 2], 0); // MS
    data[lbaf0 + 2] = 12; // DS = 2^12 = 4096
    data[lbaf0 + 3] = 0; // RP

    data
}

fn write_padded_ascii(buf: &mut [u8], value: &str) {
    let bytes = value.as_bytes();
    let len = buf.len().min(bytes.len());
    buf[..len].copy_from_slice(&bytes[..len]);
    if len < buf.len() {
        buf[len..].fill(b' ');
    }
}

// ============================================================================//
// Tests                                                                        //
// ============================================================================//

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    #[test]
    fn config_defaults_are_sane() {
        let cfg = NvmeofSimConfig::default();
        assert_eq!(cfg.listen_port, 4420);
        assert_eq!(cfg.listen_addr, "0.0.0.0");
    }

    #[test]
    fn creates_backing_file_if_missing() {
        let path = "target/test_backing.img";
        let _ = fs::remove_file(path);
        ensure_backing_file(path).unwrap();
        assert!(Path::new(path).exists());
        fs::remove_file(path).ok();
    }
}
