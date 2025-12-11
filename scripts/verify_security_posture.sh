#!/bin/bash
set -e

echo "🛡️  Verifying SPACE Security Posture (Phase 4)..."

echo "   Checking cargo-deny advisories..."
if cargo deny check advisories; then
    echo "   ✅ Advisories check passed (Waivers applied)."
else
    echo "   ❌ Advisories check FAILED. Review deny.toml."
    exit 1
fi

echo "   Checking dependency bans..."
if cargo deny check bans; then
    echo "   ✅ Ban graph clean."
else
    echo "   ❌ Ban graph violations detected."
    exit 1
fi

if grep -q "RUSTSEC-2025-0057" docs/security/audit-status.json; then
    echo "   ✅ Audit log contains fxhash waiver."
else
    echo "   ❌ Audit log missing fxhash waiver."
    exit 1
fi

echo "🚀 Security verification complete. Ready for merge."
