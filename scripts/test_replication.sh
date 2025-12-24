#!/bin/bash
# Test script for chain replication
#
# This script demonstrates manual testing of the replication system.
# For automated tests, see: cargo test -p foundry replication

set -e

echo "=== Chain Replication Test ==="
echo ""
echo "This test demonstrates synchronous chain replication between"
echo "a primary node and a replica node."
echo ""

# Check if foundry tests are available
echo "Running automated replication tests..."
echo ""

cd "$(dirname "$0")/.."

# Run the replication tests
cargo test -p foundry replication -- --nocapture

echo ""
echo "=== Test Complete ==="
echo ""
echo "The tests verify:"
echo "1. Handshake between primary and replica"
echo "2. Single write replication"
echo "3. Multiple write replication"
echo "4. Error handling for invalid volumes"
echo ""
echo "All writes to the primary are synchronously replicated to the replica"
echo "before being acknowledged to the client, ensuring zero RPO."
