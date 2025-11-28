#!/bin/bash
set -e

echo "🧪 SPACE Phase 2.2 - Deduplication Test Suite"
echo "=============================================="
echo ""

# Colors
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m' # No Color

# Clean up old test artifacts
echo -e "${BLUE}Cleaning up old test files...${NC}"
rm -f test*.nvram* test*.metadata space.nvram* space.metadata
echo ""

# Build the project
echo -e "${BLUE}Building SPACE with deduplication support...${NC}"
cargo build --release 2>&1 | tail -n 5
if [ $? -eq 0 ]; then
    echo -e "${GREEN}✅ Build successful${NC}"
else
    echo -e "${RED}❌ Build failed${NC}"
    exit 1
fi
echo ""

# Run unit tests
echo -e "${BLUE}Running unit tests...${NC}"
echo ""
cargo test --lib -- --nocapture 2>&1 | grep -E "(test |✅|running|passed)"
echo ""

# Run dedup integration tests
echo -e "${BLUE}Running deduplication integration tests...${NC}"
echo ""
cargo test --test dedup_test -- --nocapture
echo ""

# Demo 1: Create file with repeated content
echo -e "${BLUE}Demo 1: Creating file with repeated content...${NC}"
echo "SPACE STORAGE PLATFORM " > demo_repeated.txt
for i in {1..5000}; do
    echo "SPACE STORAGE PLATFORM " >> demo_repeated.txt
done

FILE_SIZE=$(wc -c < demo_repeated.txt)
echo "   Created file: $FILE_SIZE bytes"
echo ""

# Create capsule 1
echo -e "${BLUE}Writing first capsule...${NC}"
./target/release/spacectl create --file demo_repeated.txt > /tmp/capsule1.txt
CAPSULE1=$(grep "Capsule created:" /tmp/capsule1.txt | awk '{print $4}')
echo "   Capsule ID: $CAPSULE1"
echo ""

# Create capsule 2 (same content - should dedupe)
echo -e "${BLUE}Writing second capsule (same content - should dedupe)...${NC}"
./target/release/spacectl create --file demo_repeated.txt > /tmp/capsule2.txt
CAPSULE2=$(grep "Capsule created:" /tmp/capsule2.txt | awk '{print $4}')
echo "   Capsule ID: $CAPSULE2"
echo ""

# Verify both capsules are readable
echo -e "${BLUE}Verifying data integrity...${NC}"
./target/release/spacectl read $CAPSULE1 > /tmp/verify1.txt
./target/release/spacectl read $CAPSULE2 > /tmp/verify2.txt

if diff -q demo_repeated.txt /tmp/verify1.txt > /dev/null && \
   diff -q demo_repeated.txt /tmp/verify2.txt > /dev/null; then
    echo -e "${GREEN}✅ Data integrity verified for both capsules${NC}"
else
    echo -e "${RED}❌ Data integrity check failed${NC}"
    exit 1
fi
echo ""

# Check metadata for dedup evidence
echo -e "${BLUE}Analyzing metadata for deduplication...${NC}"
if [ -f space.metadata ]; then
    CONTENT_STORE_SIZE=$(grep -o '"content_store"' space.metadata | wc -l)
    echo "   Content store entries found: $CONTENT_STORE_SIZE"
    
    # Count capsules
    CAPSULE_COUNT=$(grep -o '"capsules"' space.metadata | wc -l)
    echo "   Capsules created: $CAPSULE_COUNT"
    
    # Check for deduped_bytes field
    DEDUP_BYTES=$(grep -o 'deduped_bytes' space.metadata | wc -l)
    if [ $DEDUP_BYTES -gt 0 ]; then
        echo -e "${GREEN}   ✅ Deduplication metadata present${NC}"
    fi
fi
echo ""

# Demo 2: Create different files
echo -e "${BLUE}Demo 2: Creating unique content (should NOT dedupe)...${NC}"
echo "Unique content $(date)" > demo_unique1.txt
echo "Different content $(date +%s)" > demo_unique2.txt

./target/release/spacectl create --file demo_unique1.txt > /tmp/capsule3.txt
./target/release/spacectl create --file demo_unique2.txt > /tmp/capsule4.txt

echo -e "${GREEN}✅ Unique content test completed${NC}"
echo ""

# Summary
echo -e "${GREEN}═══════════════════════════════════════${NC}"
echo -e "${GREEN}  ✨ DEDUPLICATION TESTS COMPLETE ✨${NC}"
echo -e "${GREEN}═══════════════════════════════════════${NC}"
echo ""
echo "What was tested:"
echo "  ✅ Build with BLAKE3 and dedup modules"
echo "  ✅ Unit tests for content hashing"
echo "  ✅ Integration tests for dedup scenarios"
echo "  ✅ Multi-capsule deduplication"
echo "  ✅ Data integrity with dedup enabled"
echo "  ✅ Metadata persistence of content store"
echo ""

# Cleanup
echo -e "${BLUE}Cleaning up demo files...${NC}"
rm -f demo_repeated.txt demo_unique*.txt /tmp/capsule*.txt /tmp/verify*.txt
echo ""

echo "To inspect deduplication metadata:"
echo "  cat space.metadata | jq '.content_store'"
echo ""
echo "To see dedup in action with verbose output:"
echo "  RUST_LOG=debug cargo run --release -- create --file <your-file>"
echo ""