# Orbit Command Interface Mockup

This is a static Vite/React build of the Orbit command interface concept pulled from Figma. It has no backend wiring; it is just the visual mock so designers and engineers can explore the layout.

## Prerequisites

- Node.js 18+ and `npm` available on your PATH.
- No other project services are required. The mock runs as a local Vite dev server.

## Quick start (recommended)

```bash
./scripts/view_ui_mockup.sh
```

- Installs dependencies inside `UI_mockup/` on first run.
- Launches Vite on `http://localhost:4173` (override with `PORT=3000 ./scripts/view_ui_mockup.sh`).
- Press `Ctrl+C` to stop.

## Manual steps

```bash
cd UI_mockup
npm install
npm run dev -- --host --port 4173 --open
```

Open the printed localhost URL if it does not auto-open. Expect placeholder data only—interactive flows are not connected to SPACE services.
