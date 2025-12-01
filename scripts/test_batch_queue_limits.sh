#!/bin/bash

# scripts/test_batch_queue_limits.sh
#
# Runs the specific unit tests for BatchQueue resource limits.
# Verifies that the queue flushes correctly on byte-size thresholds
# to prevent OOM, in addition to count and time thresholds.

set -e

# Colors for output
GREEN='\033[0;32m'
RED='\033[0;31m'
NC='\033[0m' # No Color

echo -e "${GREEN}=== Starting BatchQueue Resource Limit Tests ===${NC}"

# Ensure we are in the project root
if [ ! -f "Cargo.toml" ]; then
    echo -e "${RED}Error: Please run this script from the project root.${NC}"
    exit 1
fi

# 1. Test the Byte Limit Trigger
# This confirms that a small number of large items triggers a flush
echo -e "\n${GREEN}[1/3] Testing Byte-Size Limit Trigger...${NC}"
if cargo test -p scaling --lib batch_queue::tests::test_batch_queue_byte_limit -- --nocapture; then
    echo -e "${GREEN}>>> Byte Limit Test PASSED${NC}"
else
    echo -e "${RED}>>> Byte Limit Test FAILED${NC}"
    exit 1
fi

# 2. Test the Count Limit Trigger (Regression Check)
# This confirms that small items still flush when they hit the count limit
echo -e "\n${GREEN}[2/3] Testing Count Limit Trigger...${NC}"
if cargo test -p scaling --lib batch_queue::tests::test_batch_queue_size_limit -- --nocapture; then
    echo -e "${GREEN}>>> Count Limit Test PASSED${NC}"
else
    echo -e "${RED}>>> Count Limit Test FAILED${NC}"
    exit 1
fi

# 3. Test Queue Statistics
# Confirms that the stats reporter correctly calculates byte usage
echo -e "\n${GREEN}[3/3] Testing Queue Statistics Reporting...${NC}"
if cargo test -p scaling --lib batch_queue::tests::test_queue_stats -- --nocapture; then
    echo -e "${GREEN}>>> Stats Reporting Test PASSED${NC}"
else
    echo -e "${RED}>>> Stats Reporting Test FAILED${NC}"
    exit 1
fi

echo -e "\n${GREEN}=== All BatchQueue Limit Tests Passed Successfully ===${NC}"
