#!/bin/bash

# scripts/test_read_range.sh
# Verifies the correctness of the native read_range implementation.

set -e

GREEN='\033[0;32m'
RED='\033[0;31m'
NC='\033[0m'

echo -e "${GREEN}=== Starting Range Read Tests ===${NC}"

if [ ! -f "Cargo.toml" ]; then
    echo -e "${RED}Error: Run from project root.${NC}"
    exit 1
fi

echo -e "\n${GREEN}[1/1] Verification of Range Read Logic...${NC}"

if cargo test -p capsule-registry --test range_read_test -- --nocapture; then
    echo -e "${GREEN}>>> Range Read Verification PASSED${NC}"
else
    echo -e "${RED}>>> Range Read Verification FAILED${NC}"
    exit 1
fi

echo -e "\n${GREEN}=== Range Read Implementation Verified ===${NC}"
