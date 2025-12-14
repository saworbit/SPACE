import { useState, useEffect, useRef } from 'react';
import { motion } from 'motion/react';
import { Terminal as TerminalIcon } from 'lucide-react';

interface HistoryEntry {
  type: 'command' | 'output' | 'error';
  content: string;
}

export function Terminal({ onClose }: { onClose: () => void }) {
  const [history, setHistory] = useState<HistoryEntry[]>([
    { type: 'output', content: 'Orbit Command Terminal v1.0' },
    { type: 'output', content: 'Type "help" for available commands' },
    { type: 'output', content: '' }
  ]);
  const [input, setInput] = useState('');
  const inputRef = useRef<HTMLInputElement>(null);
  const historyEndRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
    historyEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [history]);

  const executeCommand = (cmd: string) => {
    const trimmedCmd = cmd.trim();
    setHistory(prev => [...prev, { type: 'command', content: `$ ${trimmedCmd}` }]);

    if (!trimmedCmd) {
      setHistory(prev => [...prev, { type: 'output', content: '' }]);
      return;
    }

    const parts = trimmedCmd.split(' ');
    const command = parts[0].toLowerCase();

    let output: HistoryEntry[] = [];

    switch (command) {
      case 'help':
        output = [
          { type: 'output', content: 'Available commands:' },
          { type: 'output', content: '  status        - Show cluster status' },
          { type: 'output', content: '  nodes         - List all nodes' },
          { type: 'output', content: '  deploy <name> - Deploy a new capsule' },
          { type: 'output', content: '  zones         - Show zone information' },
          { type: 'output', content: '  keys          - Key rotation status' },
          { type: 'output', content: '  clear         - Clear terminal' },
          { type: 'output', content: '  help          - Show this help' },
          { type: 'output', content: '' }
        ];
        break;

      case 'status':
        output = [
          { type: 'output', content: 'Cluster Status: HEALTHY' },
          { type: 'output', content: 'Nodes: 5 (1 leader, 4 followers)' },
          { type: 'output', content: 'IOPS: 68,432' },
          { type: 'output', content: 'Latency: 0.73ms' },
          { type: 'output', content: 'Federation Health: 98%' },
          { type: 'output', content: '' }
        ];
        break;

      case 'nodes':
        output = [
          { type: 'output', content: 'ID  NAME           ROLE      STATUS    LATENCY' },
          { type: 'output', content: '1   node-alpha     LEADER    HEALTHY   0.0ms' },
          { type: 'output', content: '2   node-beta      FOLLOWER  HEALTHY   12.0ms' },
          { type: 'output', content: '3   node-gamma     FOLLOWER  HEALTHY   8.0ms' },
          { type: 'output', content: '4   node-delta     FOLLOWER  LAGGING   45.0ms' },
          { type: 'output', content: '5   node-epsilon   FOLLOWER  HEALTHY   15.0ms' },
          { type: 'output', content: '' }
        ];
        break;

      case 'deploy':
        if (parts.length < 2) {
          output = [
            { type: 'error', content: 'Error: Missing capsule name' },
            { type: 'error', content: 'Usage: deploy <name>' },
            { type: 'output', content: '' }
          ];
        } else {
          output = [
            { type: 'output', content: `Deploying capsule: ${parts[1]}` },
            { type: 'output', content: 'Creating namespace...' },
            { type: 'output', content: 'Allocating zones...' },
            { type: 'output', content: 'Configuring pipeline...' },
            { type: 'output', content: `✓ Capsule ${parts[1]} deployed successfully` },
            { type: 'output', content: '' }
          ];
        }
        break;

      case 'zones':
        output = [
          { type: 'output', content: 'Total Zones: 400' },
          { type: 'output', content: 'Sequential Write: 120 (30%)' },
          { type: 'output', content: 'Cold Data: 200 (50%)' },
          { type: 'output', content: 'GC Pressure: 80 (20%)' },
          { type: 'output', content: 'Average Wear: 42.3%' },
          { type: 'output', content: '' }
        ];
        break;

      case 'keys':
        output = [
          { type: 'output', content: 'Encryption: Kyber-1024' },
          { type: 'output', content: 'Current Key Age: 2847s' },
          { type: 'output', content: 'Time to Rotation: 753s' },
          { type: 'output', content: 'Last Rotation: 47m ago' },
          { type: 'output', content: '' }
        ];
        break;

      case 'clear':
        setHistory([
          { type: 'output', content: 'Orbit Command Terminal v1.0' },
          { type: 'output', content: 'Type "help" for available commands' },
          { type: 'output', content: '' }
        ]);
        setInput('');
        return;

      default:
        output = [
          { type: 'error', content: `Command not found: ${command}` },
          { type: 'error', content: 'Type "help" for available commands' },
          { type: 'output', content: '' }
        ];
    }

    setHistory(prev => [...prev, ...output]);
    setInput('');
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Enter') {
      executeCommand(input);
    }
  };

  return (
    <motion.div
      className="fixed top-0 left-0 right-0 h-2/3 bg-black/95 backdrop-blur-md border-b-2 border-neon-cyan shadow-2xl z-50"
      initial={{ y: '-100%' }}
      animate={{ y: 0 }}
      exit={{ y: '-100%' }}
      transition={{ type: 'spring', damping: 25, stiffness: 200 }}
    >
      {/* Header */}
      <div className="flex items-center justify-between px-6 py-3 border-b border-neon-cyan/30">
        <div className="flex items-center gap-2">
          <TerminalIcon className="w-5 h-5 text-neon-cyan" />
          <span className="text-neon-cyan tracking-wider">SPACECTL TERMINAL</span>
        </div>
        <button
          onClick={onClose}
          className="text-white/60 hover:text-white transition-colors"
        >
          Close [~]
        </button>
      </div>

      {/* Terminal Content */}
      <div className="h-[calc(100%-3.5rem)] overflow-auto p-6 font-mono text-sm">
        {history.map((entry, index) => (
          <div
            key={index}
            className={
              entry.type === 'command' ? 'text-neon-cyan' :
              entry.type === 'error' ? 'text-supernova-orange' :
              'text-white/80'
            }
          >
            {entry.content}
          </div>
        ))}
        
        {/* Input Line */}
        <div className="flex items-center gap-2 mt-2">
          <span className="text-neon-cyan">$</span>
          <input
            ref={inputRef}
            type="text"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={handleKeyDown}
            className="flex-1 bg-transparent border-none outline-none text-white caret-neon-cyan"
            autoFocus
          />
        </div>
        
        <div ref={historyEndRef} />
      </div>

      {/* Cursor Blink Animation */}
      <style>{`
        @keyframes blink {
          0%, 50% { opacity: 1; }
          51%, 100% { opacity: 0; }
        }
        input:focus {
          caret-color: var(--color-neon-cyan);
        }
      `}</style>
    </motion.div>
  );
}
