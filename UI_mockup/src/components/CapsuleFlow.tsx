import { useState } from 'react';
import { motion } from 'motion/react';
import { Lock, Minimize2, FileArchive, Play, Settings } from 'lucide-react';

interface FlowNode {
  id: string;
  type: 'input' | 'transform' | 'output';
  label: string;
  icon: React.ReactNode;
  position: { x: number; y: number };
  throughput: number;
}

interface Connection {
  from: string;
  to: string;
  active: boolean;
}

export function CapsuleFlow() {
  const [hoveredNode, setHoveredNode] = useState<string | null>(null);
  const [hoveredConnection, setHoveredConnection] = useState<string | null>(null);

  const nodes: FlowNode[] = [
    { id: 'input', type: 'input', label: 'Data Source', icon: <Play className="w-5 h-5" />, position: { x: 100, y: 200 }, throughput: 125.5 },
    { id: 'compress', type: 'transform', label: 'Compression', icon: <FileArchive className="w-5 h-5" />, position: { x: 300, y: 150 }, throughput: 98.2 },
    { id: 'dedup', type: 'transform', label: 'Deduplication', icon: <Minimize2 className="w-5 h-5" />, position: { x: 300, y: 250 }, throughput: 87.4 },
    { id: 'encrypt', type: 'transform', label: 'Encryption', icon: <Lock className="w-5 h-5" />, position: { x: 500, y: 200 }, throughput: 102.8 },
    { id: 'output', type: 'output', label: 'Storage', icon: <Settings className="w-5 h-5" />, position: { x: 700, y: 200 }, throughput: 95.3 }
  ];

  const connections: Connection[] = [
    { from: 'input', to: 'compress', active: true },
    { from: 'input', to: 'dedup', active: true },
    { from: 'compress', to: 'encrypt', active: true },
    { from: 'dedup', to: 'encrypt', active: true },
    { from: 'encrypt', to: 'output', active: true }
  ];

  const getNodePosition = (nodeId: string) => {
    const node = nodes.find(n => n.id === nodeId);
    return node ? node.position : { x: 0, y: 0 };
  };

  return (
    <div className="w-full h-full relative overflow-hidden bg-void">
      {/* Grid Background */}
      <div className="absolute inset-0">
        <svg className="w-full h-full">
          <defs>
            <pattern id="capsule-grid" width="30" height="30" patternUnits="userSpaceOnUse">
              <circle cx="1" cy="1" r="1" fill="currentColor" className="text-neon-cyan/20" />
            </pattern>
          </defs>
          <rect width="100%" height="100%" fill="url(#capsule-grid)" />
        </svg>
      </div>

      {/* Title */}
      <div className="absolute top-8 left-8 z-10">
        <h1 className="text-neon-cyan text-2xl tracking-wider">CAPSULEFLOW</h1>
        <p className="text-white/40 text-sm mt-1">Pipeline Orchestrator // CAD for Data Streams</p>
      </div>

      {/* Canvas */}
      <div className="absolute inset-0 top-24">
        <svg className="absolute inset-0 w-full h-full pointer-events-none">
          {/* Connection Lines */}
          {connections.map((conn, index) => {
            const from = getNodePosition(conn.from);
            const to = getNodePosition(conn.to);
            const connectionId = `${conn.from}-${conn.to}`;
            const isHovered = hoveredConnection === connectionId;

            // Calculate control points for curved line
            const midX = (from.x + to.x) / 2;
            const path = `M ${from.x + 100} ${from.y + 40} Q ${midX} ${from.y + 40}, ${midX} ${(from.y + to.y) / 2 + 40} T ${to.x} ${to.y + 40}`;

            return (
              <g key={connectionId}>
                {/* Invisible wider line for easier hovering */}
                <motion.path
                  d={path}
                  fill="none"
                  stroke="transparent"
                  strokeWidth="20"
                  className="pointer-events-auto cursor-pointer"
                  onHoverStart={() => setHoveredConnection(connectionId)}
                  onHoverEnd={() => setHoveredConnection(null)}
                />
                
                {/* Visible line */}
                <motion.path
                  d={path}
                  fill="none"
                  stroke="currentColor"
                  strokeWidth={isHovered ? "3" : "2"}
                  className={isHovered ? "text-neon-cyan" : "text-nebula-purple/60"}
                  initial={{ pathLength: 0 }}
                  animate={{ pathLength: 1 }}
                  transition={{ duration: 1, delay: index * 0.2 }}
                />

                {/* Animated flow particles */}
                {conn.active && (
                  <>
                    <motion.circle
                      r="4"
                      fill="currentColor"
                      className="text-neon-cyan"
                    >
                      <animateMotion
                        dur="3s"
                        repeatCount="indefinite"
                        path={path}
                      />
                    </motion.circle>
                    <motion.circle
                      r="3"
                      fill="currentColor"
                      className="text-nebula-purple"
                    >
                      <animateMotion
                        dur="3s"
                        repeatCount="indefinite"
                        path={path}
                        begin="1s"
                      />
                    </motion.circle>
                  </>
                )}
              </g>
            );
          })}
        </svg>

        {/* Nodes */}
        {nodes.map((node, index) => (
          <motion.div
            key={node.id}
            className="absolute"
            style={{
              left: node.position.x,
              top: node.position.y
            }}
            initial={{ scale: 0, opacity: 0 }}
            animate={{ scale: 1, opacity: 1 }}
            transition={{ delay: index * 0.1 }}
            onHoverStart={() => setHoveredNode(node.id)}
            onHoverEnd={() => setHoveredNode(null)}
          >
            <motion.div
              className={`relative w-24 h-20 bg-glass border-2 rounded-lg flex flex-col items-center justify-center gap-2 cursor-pointer ${
                node.type === 'input' ? 'border-neon-cyan' :
                node.type === 'output' ? 'border-neon-cyan' :
                'border-nebula-purple'
              } ${hoveredNode === node.id ? 'glow-cyan' : ''}`}
              whileHover={{ scale: 1.05 }}
              whileTap={{ scale: 0.95 }}
            >
              <div className={
                node.type === 'input' ? 'text-neon-cyan' :
                node.type === 'output' ? 'text-neon-cyan' :
                'text-nebula-purple'
              }>
                {node.icon}
              </div>
              <span className="text-white text-xs">{node.label}</span>
              
              {/* Throughput indicator */}
              <div className="absolute -bottom-6 left-1/2 -translate-x-1/2 text-white/40 text-[10px] whitespace-nowrap">
                {node.throughput} MB/s
              </div>

              {/* Pulse effect */}
              <motion.div
                className={`absolute inset-0 rounded-lg border-2 ${
                  node.type === 'input' || node.type === 'output' ? 'border-neon-cyan' : 'border-nebula-purple'
                }`}
                animate={{
                  scale: [1, 1.2, 1],
                  opacity: [0.5, 0, 0.5]
                }}
                transition={{
                  duration: 2,
                  repeat: Infinity,
                  ease: "easeOut"
                }}
              />
            </motion.div>

            {/* Hover tooltip */}
            {hoveredNode === node.id && (
              <motion.div
                className="absolute top-full mt-4 left-1/2 -translate-x-1/2 bg-glass border border-neon-cyan/30 rounded px-3 py-2 whitespace-nowrap z-20"
                initial={{ opacity: 0, y: -10 }}
                animate={{ opacity: 1, y: 0 }}
              >
                <div className="text-white text-sm">{node.label}</div>
                <div className="text-white/60 text-xs mt-1">Throughput: {node.throughput} MB/s</div>
                <div className="text-neon-cyan text-xs mt-1">Status: ACTIVE</div>
              </motion.div>
            )}
          </motion.div>
        ))}

        {/* X-Ray Mode Tooltip for hovered connection */}
        {hoveredConnection && (
          <motion.div
            className="absolute bg-glass border border-neon-cyan rounded p-4 z-30"
            style={{
              left: '50%',
              top: '50%',
              transform: 'translate(-50%, -50%)'
            }}
            initial={{ opacity: 0, scale: 0.9 }}
            animate={{ opacity: 1, scale: 1 }}
          >
            <div className="text-neon-cyan text-sm mb-2">LIVE STREAM DATA</div>
            <div className="font-mono text-xs text-white/80 space-y-1">
              <div>0x4A 0x73 0x6F 0x6E 0x20 0x64 0x61 0x74</div>
              <div>0x61 0x20 0x73 0x74 0x72 0x65 0x61 0x6D</div>
              <div>0x69 0x6E 0x67 0x2E 0x2E 0x2E 0x00 0x00</div>
            </div>
            <div className="text-white/40 text-xs mt-2">Connection: {hoveredConnection}</div>
          </motion.div>
        )}
      </div>
    </div>
  );
}
