#!/bin/bash
set -euo pipefail

# SoftRoCE brings up a software RDMA device for CI and dev environments without NICs.
modprobe rdma_rxe

# Default to binding SoftRoCE to the primary Ethernet interface.
PRIMARY_IFACE=${1:-eth0}
rdma link add rxe0 type rxe netdev "${PRIMARY_IFACE}"

# Show discovered RDMA devices.
ibv_devinfo
