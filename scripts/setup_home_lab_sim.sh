#!/bin/bash
# Setup Script for SPACE Home Lab Simulation Environment
#
# This script automates the setup of a local development/testing environment
# for SPACE, including Docker image builds, hugepages configuration, and
# container orchestration via Docker Compose.
#
# Prerequisites:
#   - Docker 25+ installed
#   - Docker Compose v2
#   - Linux host for NVMe-oF simulation (or WSL2 with limitations)
#   - At least 8GB RAM
#
# Usage:
#   ./scripts/setup_home_lab_sim.sh [OPTIONS]
#
# Options:
#   --skip-build        Skip Docker image builds (use existing images)
#   --no-nvmeof         Disable NVMe-oF simulation (only NVRAM)
#   --clean             Clean existing containers/volumes before starting
#   --help              Show this help message

set -euo pipefail

# ============================================================================
# Configuration
# ============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
cd "$PROJECT_ROOT"

# Default options
SKIP_BUILD=false
NO_NVMEOF=false
CLEAN=false

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --skip-build)
            SKIP_BUILD=true
            shift
            ;;
        --no-nvmeof)
            NO_NVMEOF=true
            shift
            ;;
        --clean)
            CLEAN=true
            shift
            ;;
        --help)
            head -n 30 "$0" | grep "^#" | sed 's/^# //'
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            echo "Run with --help for usage"
            exit 1
            ;;
    esac
done

# ============================================================================
# Functions
# ============================================================================

log_info() {
    echo -e "\n\033[1;34m[INFO]\033[0m $1"
}

log_warn() {
    echo -e "\n\033[1;33m[WARN]\033[0m $1"
}

log_error() {
    echo -e "\n\033[1;31m[ERROR]\033[0m $1"
}

log_success() {
    echo -e "\n\033[1;32m[SUCCESS]\033[0m $1"
}

check_prerequisites() {
    log_info "Checking prerequisites..."

    # Docker
    if ! command -v docker &> /dev/null; then
        log_error "Docker not found. Please install Docker 25+."
        exit 1
    fi
    local docker_version=$(docker version --format '{{.Server.Version}}' 2>/dev/null || echo "0.0.0")
    log_info "Docker version: $docker_version"

    # Docker Compose
    if ! docker compose version &> /dev/null; then
        log_error "Docker Compose v2 not found. Please install or upgrade Docker."
        exit 1
    fi
    log_info "Docker Compose: $(docker compose version)"

    # Check if on Linux (for hugepages)
    if [[ "$OSTYPE" != "linux-gnu"* ]]; then
        log_warn "Not running on Linux. NVMe-oF simulation may have limited functionality."
        if [ "$NO_NVMEOF" = false ]; then
            log_warn "Consider using --no-nvmeof flag on non-Linux systems."
        fi
    fi

    log_success "Prerequisites check passed"
}

setup_hugepages() {
    if [ "$NO_NVMEOF" = true ]; then
        log_info "Skipping hugepages setup (NVMe-oF disabled)"
        return
    fi

    log_info "Configuring hugepages for NVMe-oF simulation..."

    # Check if running on Linux
    if [[ "$OSTYPE" != "linux-gnu"* ]]; then
        log_warn "Cannot configure hugepages on non-Linux system"
        return
    fi

    # Check current hugepages
    if [ -f /proc/meminfo ]; then
        local current=$(grep "^HugePages_Total:" /proc/meminfo | awk '{print $2}')
        log_info "Current hugepages: ${current:-0}"

        if [ "${current:-0}" -lt 1024 ]; then
            log_info "Setting hugepages to 1024 (requires sudo)..."
            if sudo sh -c "echo 1024 > /proc/sys/vm/nr_hugepages" 2>/dev/null; then
                log_success "Hugepages configured successfully"
            else
                log_warn "Failed to configure hugepages. NVMe-oF will use TCP fallback."
                log_warn "To manually configure: sudo sh -c 'echo 1024 > /proc/sys/vm/nr_hugepages'"
            fi
        else
            log_success "Hugepages already configured"
        fi
    fi
}

build_images() {
    if [ "$SKIP_BUILD" = true ]; then
        log_info "Skipping image builds (--skip-build flag)"
        return
    fi

    log_info "Building Docker images..."

    # Build core image
    log_info "Building space-core:latest..."
    docker build -t space-core:latest -f Dockerfile .

    # Build sim image
    log_info "Building space-sim:latest..."
    docker build -t space-sim:latest -f Dockerfile.sim .

    log_success "Docker images built successfully"
}

clean_environment() {
    if [ "$CLEAN" = false ]; then
        return
    fi

    log_info "Cleaning existing containers and volumes..."
    docker compose down -v 2>/dev/null || true
    log_success "Environment cleaned"
}

start_services() {
    log_info "Starting Docker Compose services..."

    # Determine which services to start
    local services=""
    if [ "$NO_NVMEOF" = true ]; then
        # Override SIM_MODULES to exclude nvmeof
        export SIM_MODULES="nvram"
        log_info "SIM_MODULES set to: nvram (NVMe-oF disabled)"
    else
        export SIM_MODULES="nvram,nvmeof"
        log_info "SIM_MODULES set to: nvram,nvmeof"
    fi

    # Start all services
    docker compose up -d

    log_success "Services started"
}

health_check() {
    log_info "Running health checks..."

    # Wait for containers to start
    sleep 5

    # Check sim container
    if docker compose ps sim | grep -q "running"; then
        log_success "Sim container is running"

        # Check sim logs for readiness
        local logs=$(docker compose logs sim 2>&1)
        if echo "$logs" | grep -q "ready\|Ready\|Started"; then
            log_success "Sim modules appear to be ready"
        else
            log_warn "Sim container running but readiness unclear. Check logs:"
            log_warn "  docker compose logs sim"
        fi
    else
        log_warn "Sim container not running. Check status:"
        log_warn "  docker compose ps sim"
    fi

    # Check core services
    local running=$(docker compose ps --filter "status=running" | grep -c "space-" || echo "0")
    log_info "Running services: $running"

    log_success "Health check complete"
}

test_nvmeof_connection() {
    if [ "$NO_NVMEOF" = true ]; then
        return
    fi

    log_info "Testing NVMe-oF connection (if nvme-cli available)..."

    if command -v nvme &> /dev/null; then
        log_info "nvme-cli detected. Testing connection to sim target..."

        # Note: This requires the container to expose the port and be fully initialized
        if nvme discover -t tcp -a 127.0.0.1 -s 4420 2>&1 | grep -q "nqn"; then
            log_success "NVMe-oF target discovered successfully"
        else
            log_warn "Could not discover NVMe-oF target. It may still be initializing."
            log_warn "To test manually: nvme discover -t tcp -a 127.0.0.1 -s 4420"
        fi
    else
        log_info "nvme-cli not installed. To test NVMe-oF, install it:"
        log_info "  Ubuntu: sudo apt install nvme-cli"
    fi
}

show_next_steps() {
    log_success "Setup complete!"
    echo ""
    echo "Next steps:"
    echo "  1. View running containers:    docker compose ps"
    echo "  2. View sim logs:              docker compose logs -f sim"
    echo "  3. Access spacectl shell:      docker compose exec spacectl sh"
    echo "  4. Run tests:                  ./scripts/test_e2e_sim.sh"
    echo "  5. Stop services:              docker compose down"
    echo ""
    echo "Simulation status:"
    echo "  - NVRAM simulation: Enabled (file-backed log)"
    if [ "$NO_NVMEOF" = false ]; then
        echo "  - NVMe-oF simulation: Enabled (TCP transport on port 4420)"
    else
        echo "  - NVMe-oF simulation: Disabled (--no-nvmeof flag)"
    fi
    echo ""
    echo "For more information, see:"
    echo "  - docs/SIMULATIONS.md"
    echo "  - docs/CONTAINERIZATION.md"
}

# ============================================================================
# Main
# ============================================================================

main() {
    echo "========================================"
    echo "SPACE Home Lab Simulation Setup"
    echo "========================================"

    check_prerequisites
    setup_hugepages
    clean_environment
    build_images
    start_services
    health_check
    test_nvmeof_connection
    show_next_steps
}

main
