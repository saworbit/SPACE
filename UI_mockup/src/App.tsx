import { useState, useEffect } from 'react';
import { InfoRail } from './components/InfoRail';
import { Dock } from './components/Dock';
import { Bridge } from './components/Bridge';
import { CapsuleFlow } from './components/CapsuleFlow';
import { Foundry } from './components/Foundry';
import { Vault } from './components/Vault';
import { CommandPalette } from './components/CommandPalette';
import { Terminal } from './components/Terminal';
import { Registry } from './components/Registry';

export type Dimension = 'bridge' | 'capsule' | 'foundry' | 'vault' | 'terminal' | 'registry';

export default function App() {
  const [activeDimension, setActiveDimension] = useState<Dimension>('bridge');
  const [commandPaletteOpen, setCommandPaletteOpen] = useState(false);
  const [terminalOpen, setTerminalOpen] = useState(false);

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      // Cmd+K or Ctrl+K for command palette
      if ((e.metaKey || e.ctrlKey) && e.key === 'k') {
        e.preventDefault();
        setCommandPaletteOpen(prev => !prev);
      }
      // ~ key for terminal
      if (e.key === '`' && !commandPaletteOpen) {
        e.preventDefault();
        setTerminalOpen(prev => !prev);
      }
      // Escape to close everything
      if (e.key === 'Escape') {
        setCommandPaletteOpen(false);
        setTerminalOpen(false);
      }
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [commandPaletteOpen]);

  return (
    <div className="relative w-screen h-screen overflow-hidden bg-void">
      {/* Info Rail */}
      <InfoRail />

      {/* Dock */}
      <Dock 
        activeDimension={activeDimension}
        onDimensionChange={setActiveDimension}
      />

      {/* Main Viewport */}
      <div className="absolute top-12 left-20 right-0 bottom-0 overflow-hidden">
        {activeDimension === 'bridge' && <Bridge />}
        {activeDimension === 'capsule' && <CapsuleFlow />}
        {activeDimension === 'foundry' && <Foundry />}
        {activeDimension === 'vault' && <Vault />}
        {activeDimension === 'registry' && <Registry />}
      </div>

      {/* Command Palette */}
      {commandPaletteOpen && (
        <CommandPalette 
          onClose={() => setCommandPaletteOpen(false)}
          onDimensionChange={setActiveDimension}
        />
      )}

      {/* Terminal */}
      {terminalOpen && (
        <Terminal onClose={() => setTerminalOpen(false)} />
      )}

      {/* Keyboard hint */}
      <div className="absolute bottom-4 right-4 text-cyan-500/30 text-xs flex gap-4">
        <span>⌘K Command Palette</span>
        <span>~ Terminal</span>
      </div>
    </div>
  );
}