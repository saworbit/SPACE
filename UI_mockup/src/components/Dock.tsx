import { Globe, Workflow, Database, Shield, BookOpen, type LucideIcon } from 'lucide-react';
import { motion } from 'motion/react';
import type { Dimension } from '../App';

interface DockItem {
  id: Dimension;
  icon: LucideIcon;
  label: string;
  health: 'healthy' | 'processing' | 'critical';
}

const dockItems: DockItem[] = [
  { id: 'bridge', icon: Globe, label: 'Bridge', health: 'healthy' },
  { id: 'capsule', icon: Workflow, label: 'CapsuleFlow', health: 'processing' },
  { id: 'foundry', icon: Database, label: 'Foundry', health: 'healthy' },
  { id: 'vault', icon: Shield, label: 'Vault', health: 'healthy' },
  { id: 'registry', icon: BookOpen, label: 'Registry', health: 'healthy' }
];

interface DockProps {
  activeDimension: Dimension;
  onDimensionChange: (dimension: Dimension) => void;
}

export function Dock({ activeDimension, onDimensionChange }: DockProps) {
  const healthColors = {
    healthy: 'text-cyan-500',
    processing: 'text-purple-500',
    critical: 'text-orange-500'
  };

  return (
    <motion.div 
      className="absolute left-0 top-12 bottom-0 w-20 bg-void/90 backdrop-blur-xl border-r border-cyan-500/20 flex flex-col items-center py-6 gap-4"
      initial={{ x: -80 }}
      animate={{ x: 0 }}
      transition={{ type: 'spring', stiffness: 300, damping: 30 }}
    >
      {dockItems.map((item) => {
        const Icon = item.icon;
        const isActive = activeDimension === item.id;
        
        return (
          <motion.button
            key={item.id}
            onClick={() => onDimensionChange(item.id)}
            className={`relative w-14 h-14 rounded-xl flex items-center justify-center transition-colors ${
              isActive 
                ? 'bg-cyan-500/20 border-2 border-cyan-500/50' 
                : 'bg-void/50 border border-cyan-500/10 hover:border-cyan-500/30'
            }`}
            whileHover={{ scale: 1.1 }}
            whileTap={{ scale: 0.95 }}
          >
            <Icon className={`w-6 h-6 ${isActive ? 'text-cyan-500' : healthColors[item.health]}`} />
            
            {/* Health indicator dot */}
            <motion.div
              className={`absolute -bottom-1 -right-1 w-2 h-2 rounded-full ${
                item.health === 'healthy' ? 'bg-cyan-500' :
                item.health === 'processing' ? 'bg-purple-500' :
                'bg-orange-500'
              }`}
              animate={{
                scale: [1, 1.2, 1],
                opacity: [1, 0.7, 1]
              }}
              transition={{
                duration: 2,
                repeat: Infinity,
                ease: 'easeInOut'
              }}
            />
            
            {/* Tooltip */}
            <div className="absolute left-full ml-4 px-3 py-1 bg-void/95 border border-cyan-500/30 rounded text-cyan-500 text-xs whitespace-nowrap opacity-0 hover:opacity-100 transition-opacity pointer-events-none">
              {item.label}
            </div>
          </motion.button>
        );
      })}
    </motion.div>
  );
}
