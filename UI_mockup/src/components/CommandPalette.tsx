import { useState, useEffect, useRef } from 'react';
import { motion, AnimatePresence } from 'motion/react';
import { Search, Rocket, Skull, Key, Play, StopCircle, RefreshCw } from 'lucide-react';
import type { Dimension } from '../App';

interface Command {
  id: string;
  label: string;
  icon: React.ReactNode;
  action: () => void;
  category: string;
}

export function CommandPalette({ 
  onClose, 
  onDimensionChange 
}: { 
  onClose: () => void;
  onDimensionChange: (dimension: Dimension) => void;
}) {
  const [query, setQuery] = useState('');
  const [selectedIndex, setSelectedIndex] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  const commands: Command[] = [
    {
      id: 'deploy-capsule',
      label: 'Deploy Capsule',
      icon: <Rocket className="w-4 h-4" />,
      action: () => {
        console.log('Deploying capsule...');
        onDimensionChange('capsule');
        onClose();
      },
      category: 'Actions'
    },
    {
      id: 'kill-node',
      label: 'Kill Node 3',
      icon: <Skull className="w-4 h-4" />,
      action: () => {
        console.log('Terminating node 3...');
        onDimensionChange('bridge');
        onClose();
      },
      category: 'Actions'
    },
    {
      id: 'rotate-keys',
      label: 'Rotate Kyber Keys',
      icon: <Key className="w-4 h-4" />,
      action: () => {
        console.log('Rotating encryption keys...');
        onDimensionChange('vault');
        onClose();
      },
      category: 'Security'
    },
    {
      id: 'start-rebalance',
      label: 'Start Zone Rebalance',
      icon: <RefreshCw className="w-4 h-4" />,
      action: () => {
        console.log('Starting zone rebalance...');
        onDimensionChange('foundry');
        onClose();
      },
      category: 'Storage'
    },
    {
      id: 'view-bridge',
      label: 'View Bridge',
      icon: <Play className="w-4 h-4" />,
      action: () => {
        onDimensionChange('bridge');
        onClose();
      },
      category: 'Navigation'
    },
    {
      id: 'view-capsule',
      label: 'View CapsuleFlow',
      icon: <Play className="w-4 h-4" />,
      action: () => {
        onDimensionChange('capsule');
        onClose();
      },
      category: 'Navigation'
    },
    {
      id: 'view-foundry',
      label: 'View Foundry',
      icon: <Play className="w-4 h-4" />,
      action: () => {
        onDimensionChange('foundry');
        onClose();
      },
      category: 'Navigation'
    },
    {
      id: 'view-vault',
      label: 'View Vault',
      icon: <Play className="w-4 h-4" />,
      action: () => {
        onDimensionChange('vault');
        onClose();
      },
      category: 'Navigation'
    },
    {
      id: 'view-registry',
      label: 'View Registry',
      icon: <Play className="w-4 h-4" />,
      action: () => {
        onDimensionChange('registry');
        onClose();
      },
      category: 'Navigation'
    }
  ];

  // Fuzzy search
  const filteredCommands = commands.filter(cmd =>
    cmd.label.toLowerCase().includes(query.toLowerCase()) ||
    cmd.category.toLowerCase().includes(query.toLowerCase())
  );

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        setSelectedIndex(prev => (prev + 1) % filteredCommands.length);
      } else if (e.key === 'ArrowUp') {
        e.preventDefault();
        setSelectedIndex(prev => (prev - 1 + filteredCommands.length) % filteredCommands.length);
      } else if (e.key === 'Enter' && filteredCommands[selectedIndex]) {
        e.preventDefault();
        filteredCommands[selectedIndex].action();
      }
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [filteredCommands, selectedIndex]);

  // Reset selected index when query changes
  useEffect(() => {
    setSelectedIndex(0);
  }, [query]);

  return (
    <motion.div
      className="absolute inset-0 bg-black/80 backdrop-blur-sm flex items-start justify-center pt-32 z-50"
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      exit={{ opacity: 0 }}
      onClick={onClose}
    >
      <motion.div
        className="w-full max-w-2xl bg-glass border border-neon-cyan/30 rounded-lg overflow-hidden glow-cyan"
        initial={{ scale: 0.9, y: -20 }}
        animate={{ scale: 1, y: 0 }}
        exit={{ scale: 0.9, y: -20 }}
        onClick={(e) => e.stopPropagation()}
      >
        {/* Search Input */}
        <div className="flex items-center gap-3 px-4 py-3 border-b border-white/10">
          <Search className="w-5 h-5 text-neon-cyan" />
          <input
            ref={inputRef}
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Type a command or search..."
            className="flex-1 bg-transparent border-none outline-none text-white placeholder-white/30"
          />
          <span className="text-white/30 text-xs">ESC</span>
        </div>

        {/* Commands List */}
        <div className="max-h-96 overflow-y-auto">
          {filteredCommands.length === 0 ? (
            <div className="px-4 py-8 text-center text-white/40">
              No commands found
            </div>
          ) : (
            <AnimatePresence mode="popLayout">
              {filteredCommands.map((cmd, index) => (
                <motion.button
                  key={cmd.id}
                  className={`w-full flex items-center gap-3 px-4 py-3 transition-colors ${
                    index === selectedIndex
                      ? 'bg-neon-cyan/10 border-l-2 border-neon-cyan'
                      : 'hover:bg-white/5'
                  }`}
                  onClick={cmd.action}
                  initial={{ opacity: 0, x: -20 }}
                  animate={{ opacity: 1, x: 0 }}
                  exit={{ opacity: 0, x: -20 }}
                  transition={{ delay: index * 0.02 }}
                >
                  <div className="text-neon-cyan">{cmd.icon}</div>
                  <div className="flex-1 text-left">
                    <div className="text-white">{cmd.label}</div>
                    <div className="text-white/40 text-xs">{cmd.category}</div>
                  </div>
                  {index === selectedIndex && (
                    <div className="text-neon-cyan text-xs">⏎</div>
                  )}
                </motion.button>
              ))}
            </AnimatePresence>
          )}
        </div>

        {/* Footer */}
        <div className="px-4 py-2 border-t border-white/10 flex items-center justify-between text-xs text-white/30">
          <div className="flex gap-3">
            <span>↑↓ Navigate</span>
            <span>⏎ Execute</span>
          </div>
          <span>Instant Execution</span>
        </div>
      </motion.div>
    </motion.div>
  );
}