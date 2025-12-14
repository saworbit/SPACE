import { useEffect, useState } from 'react';
import { motion } from 'motion/react';
import { Server, Crown } from 'lucide-react';
import { Sparkline } from './Sparkline';

interface Node {
  id: number;
  name: string;
  role: 'leader' | 'follower';
  latency: number;
  capacity: number;
  throughput: number[];
  status: 'healthy' | 'lagging' | 'failed';
}

export function Bridge() {
  const [nodes, setNodes] = useState<Node[]>([
    { id: 1, name: 'node-alpha', role: 'leader', latency: 0, capacity: 2048, throughput: [], status: 'healthy' },
    { id: 2, name: 'node-beta', role: 'follower', latency: 12, capacity: 2048, throughput: [], status: 'healthy' },
    { id: 3, name: 'node-gamma', role: 'follower', latency: 8, capacity: 1024, throughput: [], status: 'healthy' },
    { id: 4, name: 'node-delta', role: 'follower', latency: 45, capacity: 512, throughput: [], status: 'lagging' },
    { id: 5, name: 'node-epsilon', role: 'follower', latency: 15, capacity: 2048, throughput: [], status: 'healthy' }
  ]);

  useEffect(() => {
    const interval = setInterval(() => {
      setNodes(prev => prev.map(node => ({
        ...node,
        latency: node.status === 'lagging' 
          ? 40 + Math.random() * 20 
          : Math.random() * 20,
        throughput: [...node.throughput.slice(-19), Math.random() * 100]
      })));
    }, 500);

    return () => clearInterval(interval);
  }, []);

  const centerX = 600;
  const centerY = 400;

  return (
    <div className="w-full h-full relative overflow-hidden">
      {/* Background grid */}
      <div className="absolute inset-0 opacity-10">
        <svg className="w-full h-full">
          <defs>
            <pattern id="grid" width="40" height="40" patternUnits="userSpaceOnUse">
              <path d="M 40 0 L 0 0 0 40" fill="none" stroke="currentColor" strokeWidth="0.5" className="text-neon-cyan" />
            </pattern>
          </defs>
          <rect width="100%" height="100%" fill="url(#grid)" />
        </svg>
      </div>

      {/* Title */}
      <div className="absolute top-8 left-8">
        <h1 className="text-neon-cyan text-2xl tracking-wider">THE BRIDGE</h1>
        <p className="text-white/40 text-sm mt-1">Cluster Telemetry // Gravitational System</p>
      </div>

      {/* Sparklines */}
      <div className="absolute top-8 right-8 flex gap-6">
        <Sparkline 
          label="CLUSTER THROUGHPUT" 
          data={nodes.reduce((acc, node) => {
            const lastValue = node.throughput[node.throughput.length - 1] || 0;
            acc[acc.length - 1] = (acc[acc.length - 1] || 0) + lastValue;
            return acc;
          }, new Array(20).fill(0))} 
          color="cyan" 
        />
        <Sparkline 
          label="PRESSURE" 
          data={new Array(20).fill(0).map(() => Math.random() * 100)} 
          color="purple" 
        />
      </div>

      {/* Solar System Visualization */}
      <svg className="absolute inset-0 w-full h-full">
        {/* Orbit paths */}
        {nodes.filter(n => n.role === 'follower').map((node, index) => {
          const radius = 80 + (node.latency * 3) + (index * 60);
          return (
            <circle
              key={`orbit-${node.id}`}
              cx={centerX}
              cy={centerY}
              r={radius}
              fill="none"
              stroke="currentColor"
              strokeWidth="1"
              className={node.status === 'lagging' ? 'text-supernova-orange/20' : 'text-neon-cyan/10'}
              strokeDasharray="4 4"
            />
          );
        })}

        {/* Connection lines */}
        {nodes.filter(n => n.role === 'follower').map((node, index) => {
          const angle = (index / nodes.filter(n => n.role === 'follower').length) * Math.PI * 2;
          const radius = 80 + (node.latency * 3) + (index * 60);
          const x = centerX + Math.cos(angle) * radius;
          const y = centerY + Math.sin(angle) * radius;

          return (
            <motion.line
              key={`line-${node.id}`}
              x1={centerX}
              y1={centerY}
              x2={x}
              y2={y}
              stroke="currentColor"
              strokeWidth="1"
              className={node.status === 'healthy' ? 'text-neon-cyan/30' : 'text-supernova-orange/30'}
              initial={{ pathLength: 0 }}
              animate={{ pathLength: 1 }}
              transition={{ duration: 1 }}
            />
          );
        })}
      </svg>

      {/* Leader Node (Sun) */}
      <motion.div
        className="absolute"
        style={{
          left: centerX - 40,
          top: centerY - 40
        }}
        animate={{
          scale: [1, 1.05, 1],
        }}
        transition={{
          duration: 3,
          repeat: Infinity,
          ease: "easeInOut"
        }}
      >
        <div className="relative w-20 h-20 rounded-full bg-neon-cyan/20 border-2 border-neon-cyan glow-cyan flex items-center justify-center">
          <Crown className="w-8 h-8 text-neon-cyan" />
          <motion.div
            className="absolute inset-0 rounded-full border-2 border-neon-cyan"
            animate={{
              scale: [1, 1.5, 1],
              opacity: [0.5, 0, 0.5]
            }}
            transition={{
              duration: 2,
              repeat: Infinity,
              ease: "easeOut"
            }}
          />
        </div>
        <div className="absolute -bottom-8 left-1/2 -translate-x-1/2 whitespace-nowrap text-center">
          <div className="text-neon-cyan text-sm">{nodes.find(n => n.role === 'leader')?.name}</div>
          <div className="text-white/40 text-xs">RAFT LEADER</div>
        </div>
      </motion.div>

      {/* Follower Nodes (Planets) */}
      {nodes.filter(n => n.role === 'follower').map((node, index) => {
        const angle = (index / nodes.filter(n => n.role === 'follower').length) * Math.PI * 2;
        const radius = 80 + (node.latency * 3) + (index * 60);
        const x = centerX + Math.cos(angle) * radius;
        const y = centerY + Math.sin(angle) * radius;
        const size = Math.max(30, (node.capacity / 2048) * 50);

        return (
          <motion.div
            key={node.id}
            className="absolute group cursor-pointer"
            style={{
              left: x - size / 2,
              top: y - size / 2,
              width: size,
              height: size
            }}
            animate={{
              x: [0, Math.random() * 4 - 2, 0],
              y: [0, Math.random() * 4 - 2, 0]
            }}
            transition={{
              duration: 3 + Math.random() * 2,
              repeat: Infinity,
              ease: "easeInOut"
            }}
            whileHover={{ scale: 1.2 }}
          >
            <div className={`w-full h-full rounded-full border-2 flex items-center justify-center ${
              node.status === 'healthy' ? 'bg-nebula-purple/20 border-nebula-purple' :
              node.status === 'lagging' ? 'bg-supernova-orange/20 border-supernova-orange glow-orange' :
              'bg-white/5 border-white/20'
            }`}>
              <Server className={`w-4 h-4 ${
                node.status === 'healthy' ? 'text-nebula-purple' :
                node.status === 'lagging' ? 'text-supernova-orange' :
                'text-white/40'
              }`} />
            </div>

            {/* Tooltip */}
            <div className="absolute left-1/2 -translate-x-1/2 top-full mt-2 opacity-0 group-hover:opacity-100 transition-opacity pointer-events-none">
              <div className="bg-glass border border-glass px-3 py-2 rounded whitespace-nowrap">
                <div className="text-white text-sm">{node.name}</div>
                <div className="text-white/60 text-xs mt-1">Latency: {node.latency.toFixed(1)}ms</div>
                <div className="text-white/60 text-xs">Capacity: {node.capacity}GB</div>
                <div className={`text-xs mt-1 ${
                  node.status === 'healthy' ? 'text-neon-cyan' :
                  node.status === 'lagging' ? 'text-supernova-orange' :
                  'text-white/40'
                }`}>
                  {node.status.toUpperCase()}
                </div>
              </div>
            </div>
          </motion.div>
        );
      })}
    </div>
  );
}
