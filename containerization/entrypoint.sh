#!/usr/bin/env bash
set -euo pipefail

case "${1:-}" in
  s3)        exec protocol-s3 --addr 0.0.0.0:8080 "${@:2}" ;;
  registry)  exec capsule-registry --raft-addr 0.0.0.0:5000 "${@:2}" ;;
  cli)       shift; exec spacectl "$@" ;;
  *)         exec spacectl "$@" ;;
esac
