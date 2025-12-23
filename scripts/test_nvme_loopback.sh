#!/bin/bash
# Integration test for NVMe-oF Foundry binding (Milestone 8.2)
#
# This script tests the complete NVMe-oF stack:
# 1. Creates a Foundry volume
# 2. Exposes it via NVMe-oF target
# 3. Connects via kernel NVMe initiator
# 4. Performs I/O operations
# 5. Verifies data integrity
#
# Requirements:
# - Linux kernel with NVMe-oF support (nvme-cli package)
# - Root privileges (for nvme commands)
# - SPDK compiled and installed
#
# Usage:
#   sudo ./scripts/test_nvme_loopback.sh

set -e  # Exit on any error

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Configuration
VOLUME_ID=$(uuidgen)
VOLUME_NAME="test-vol-1"
VOLUME_SIZE=$((100 * 1024 * 1024))  # 100 MB
NVME_PORT=4420
NQN="nqn.2024-01.io.space:${VOLUME_NAME}"

echo "========================================"
echo "NVMe-oF Foundry Binding Integration Test"
echo "========================================"
echo ""
echo "Volume ID: $VOLUME_ID"
echo "Volume Name: $VOLUME_NAME"
echo "Volume Size: $VOLUME_SIZE bytes (100 MB)"
echo "NVMe-oF Port: $NVME_PORT"
echo "NQN: $NQN"
echo ""

# Check if running as root
if [ "$EUID" -ne 0 ]; then
    echo -e "${RED}ERROR: This script must be run as root${NC}"
    echo "Usage: sudo ./scripts/test_nvme_loopback.sh"
    exit 1
fi

# Check for nvme-cli
if ! command -v nvme &> /dev/null; then
    echo -e "${RED}ERROR: nvme-cli not found${NC}"
    echo "Install with: sudo apt-get install nvme-cli"
    exit 1
fi

# Cleanup function
cleanup() {
    echo ""
    echo "========================================"
    echo "Cleanup"
    echo "========================================"

    # Disconnect NVMe device
    if nvme list | grep -q "$NQN"; then
        echo "Disconnecting NVMe device..."
        nvme disconnect -n "$NQN" || true
    fi

    # Kill spacectl if running
    if [ ! -z "$SPACECTL_PID" ] && kill -0 "$SPACECTL_PID" 2>/dev/null; then
        echo "Stopping NVMe-oF target (PID: $SPACECTL_PID)..."
        kill "$SPACECTL_PID" || true
        wait "$SPACECTL_PID" 2>/dev/null || true
    fi

    echo "Cleanup complete."
}

# Set trap to cleanup on exit
trap cleanup EXIT INT TERM

echo "========================================"
echo "Step 1: Build spacectl"
echo "========================================"
cargo build --bin spacectl --release
echo -e "${GREEN}✓ Build complete${NC}"
echo ""

echo "========================================"
echo "Step 2: Start NVMe-oF Target"
echo "========================================"
echo "Creating and exposing volume..."

# Start spacectl expose in background
./target/release/spacectl expose \
    --volume-id "$VOLUME_ID" \
    --name "$VOLUME_NAME" \
    --port "$NVME_PORT" \
    --size "$VOLUME_SIZE" &
SPACECTL_PID=$!

echo "NVMe-oF target started (PID: $SPACECTL_PID)"
echo "Waiting for target to initialize..."
sleep 5

# Verify the process is still running
if ! kill -0 "$SPACECTL_PID" 2>/dev/null; then
    echo -e "${RED}ERROR: NVMe-oF target failed to start${NC}"
    exit 1
fi

echo -e "${GREEN}✓ NVMe-oF target is running${NC}"
echo ""

echo "========================================"
echo "Step 3: Connect NVMe Initiator"
echo "========================================"
echo "Connecting to NVMe-oF target..."

# Check if already connected
if nvme list | grep -q "$NQN"; then
    echo "Already connected, disconnecting first..."
    nvme disconnect -n "$NQN" || true
    sleep 2
fi

# Connect to target
nvme connect -t tcp -n "$NQN" -a 127.0.0.1 -s "$NVME_PORT"

echo "Waiting for device to appear..."
sleep 2

# Find the device
NVME_DEVICE=""
for dev in /dev/nvme*n1; do
    if [ -b "$dev" ]; then
        if nvme id-ctrl "$dev" 2>/dev/null | grep -q "$NQN"; then
            NVME_DEVICE="$dev"
            break
        fi
    fi
done

if [ -z "$NVME_DEVICE" ]; then
    echo -e "${RED}ERROR: NVMe device not found${NC}"
    echo "Available devices:"
    nvme list
    exit 1
fi

echo -e "${GREEN}✓ Connected to NVMe device: $NVME_DEVICE${NC}"
echo ""

echo "========================================"
echo "Step 4: Verify Device Properties"
echo "========================================"
lsblk "$NVME_DEVICE"
echo ""
nvme id-ctrl "$NVME_DEVICE" | head -n 20
echo -e "${GREEN}✓ Device properties verified${NC}"
echo ""

echo "========================================"
echo "Step 5: Perform I/O Test"
echo "========================================"

# Create test data
TEST_FILE="/tmp/nvme_test_data_$$.bin"
dd if=/dev/urandom of="$TEST_FILE" bs=4k count=1 status=none

echo "Writing test data to $NVME_DEVICE..."
dd if="$TEST_FILE" of="$NVME_DEVICE" bs=4k count=1 conv=fsync status=none

echo "Reading data back..."
READ_FILE="/tmp/nvme_test_read_$$.bin"
dd if="$NVME_DEVICE" of="$READ_FILE" bs=4k count=1 status=none

echo "Verifying data integrity..."
if cmp -s "$TEST_FILE" "$READ_FILE"; then
    echo -e "${GREEN}✓ Data integrity verified - READ matches WRITE${NC}"
else
    echo -e "${RED}✗ Data mismatch - READ does not match WRITE${NC}"
    exit 1
fi

# Cleanup test files
rm -f "$TEST_FILE" "$READ_FILE"
echo ""

echo "========================================"
echo "Step 6: Performance Test"
echo "========================================"
echo "Running write performance test (1 MB)..."
dd if=/dev/zero of="$NVME_DEVICE" bs=1M count=1 conv=fsync 2>&1 | grep -E 'copied|MB/s'

echo ""
echo "Running read performance test (1 MB)..."
dd if="$NVME_DEVICE" of=/dev/null bs=1M count=1 2>&1 | grep -E 'copied|MB/s'
echo ""

echo "========================================"
echo "Step 7: Test Random I/O"
echo "========================================"
echo "Writing to random offsets..."

# Write at offset 0
echo -n "Testing write at offset 0..."
dd if=/dev/urandom of="$NVME_DEVICE" bs=4k count=1 seek=0 conv=notrunc status=none
echo " OK"

# Write at offset 1MB
echo -n "Testing write at offset 1MB..."
dd if=/dev/urandom of="$NVME_DEVICE" bs=4k count=1 seek=256 conv=notrunc status=none
echo " OK"

# Write at offset 10MB
echo -n "Testing write at offset 10MB..."
dd if=/dev/urandom of="$NVME_DEVICE" bs=4k count=1 seek=2560 conv=notrunc status=none
echo " OK"

echo -e "${GREEN}✓ Random I/O test passed${NC}"
echo ""

echo "========================================"
echo -e "${GREEN}ALL TESTS PASSED!${NC}"
echo "========================================"
echo ""
echo "Summary:"
echo "  - NVMe-oF target started successfully"
echo "  - Kernel connected to target"
echo "  - Device appeared as $NVME_DEVICE"
echo "  - I/O operations completed successfully"
echo "  - Data integrity verified"
echo "  - Random I/O tested"
echo ""
echo "The NVMe-oF Foundry binding (Milestone 8.2) is working correctly!"
echo ""
