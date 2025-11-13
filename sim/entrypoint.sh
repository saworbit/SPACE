#!/bin/bash
# Simulation Container Entrypoint
#
# This script orchestrates selective loading of simulation modules based on
# the SIM_MODULES environment variable. It's the main entrypoint for the
# "sim" container in Docker Compose.
#
# Environment Variables:
#   SIM_MODULES: Comma-separated list of modules to run (e.g., "nvram,nvmeof")
#   NODE_ID: Node identifier for multi-node sims (default: "sim-node1")
#   RUST_LOG: Log level (default: "info")
#
# Example:
#   SIM_MODULES=nvram,nvmeof NODE_ID=node1 /usr/local/bin/entrypoint.sh

set -euo pipefail

echo "=== SPACE Simulation Container ==="
echo "SIM_MODULES: ${SIM_MODULES:-none}"
echo "NODE_ID: ${NODE_ID:-sim-node1}"
echo "RUST_LOG: ${RUST_LOG:-info}"
echo "=================================="

# Parse SIM_MODULES (comma-separated)
IFS=',' read -ra MODULES <<< "${SIM_MODULES:-}"

# Track background processes
declare -a PIDS=()

# Cleanup handler
cleanup() {
    echo "Shutting down simulation modules..."
    for pid in "${PIDS[@]}"; do
        kill "$pid" 2>/dev/null || true
    done
    wait
    echo "Cleanup complete"
}
trap cleanup EXIT INT TERM

# Function to run NVRAM sim
run_nvram_sim() {
    echo "[NVRAM] Starting lightweight log simulation..."
    # NVRAM sim doesn't have a standalone binary (it's a library)
    # Create a simple marker file to indicate it's "running"
    mkdir -p /sim/nvram
    echo "NVRAM simulation ready at /sim/nvram" > /sim/nvram/status
    echo "[NVRAM] Ready (file-backed log at /sim/nvram)"
}

# Function to run NVMe-oF sim
run_nvmeof_sim() {
    echo "[NVMe-oF] Checking prerequisites..."

    # Check hugepages (warn if not available)
    if [ -f /proc/meminfo ]; then
        HUGEPAGES=$(grep "^HugePages_Total:" /proc/meminfo | awk '{print $2}')
        if [ "${HUGEPAGES:-0}" -eq 0 ]; then
            echo "[NVMe-oF] WARNING: No hugepages configured. SPDK will fall back to TCP mode."
            echo "[NVMe-oF] To enable hugepages on host: echo 1024 > /proc/sys/vm/nr_hugepages"
        else
            echo "[NVMe-oF] Hugepages available: $HUGEPAGES"
        fi
    fi

    # Start NVMe-oF simulation binary
    echo "[NVMe-oF] Starting fabric simulation..."
    NODE_ID="${NODE_ID:-sim-node1}" \
    BACKING_PATH="/sim/nvmeof/backing.img" \
    TRANSPORT="tcp" \
    LISTEN_ADDR="0.0.0.0" \
    LISTEN_PORT="4420" \
        sim-nvmeof &

    local pid=$!
    PIDS+=("$pid")
    echo "[NVMe-oF] Started with PID $pid"
}

# Function to run other sims
run_other_sim() {
    echo "[Other] Placeholder simulation module"
    echo "[Other] No-op (extend sim-other crate for GPU/ZNS/etc.)"
}

# Main: Start requested modules
if [ ${#MODULES[@]} -eq 0 ] || [ "${MODULES[0]}" == "" ]; then
    echo "No simulation modules specified (SIM_MODULES is empty)"
    echo "Valid modules: nvram, nvmeof, other"
    echo "Example: SIM_MODULES=nvram,nvmeof"
    echo ""
    echo "Running in idle mode (container stays alive for debugging)..."
    tail -f /dev/null
    exit 0
fi

for module in "${MODULES[@]}"; do
    case "$module" in
        nvram)
            run_nvram_sim
            ;;
        nvmeof)
            run_nvmeof_sim
            ;;
        other)
            run_other_sim
            ;;
        *)
            echo "Unknown module: $module (valid: nvram, nvmeof, other)"
            exit 1
            ;;
    esac
done

echo "==================================="
echo "All requested modules started"
echo "==================================="

# Wait for background processes (if any)
if [ ${#PIDS[@]} -gt 0 ]; then
    echo "Waiting for ${#PIDS[@]} background process(es)..."
    wait
else
    # If only nvram (no background procs), keep container alive
    echo "No background processes; running indefinitely..."
    tail -f /dev/null
fi
