import { motion } from 'motion/react';

export function Sparkline({ 
  label, 
  data, 
  color 
}: { 
  label: string; 
  data: number[]; 
  color: 'cyan' | 'purple' | 'orange';
}) {
  const width = 120;
  const height = 40;
  const padding = 2;

  const max = Math.max(...data, 1);
  const points = data.map((value, index) => {
    const x = (index / (data.length - 1)) * (width - padding * 2) + padding;
    const y = height - (value / max) * (height - padding * 2) - padding;
    return `${x},${y}`;
  }).join(' ');

  const colorClasses = {
    cyan: 'stroke-neon-cyan fill-neon-cyan',
    purple: 'stroke-nebula-purple fill-nebula-purple',
    orange: 'stroke-supernova-orange fill-supernova-orange'
  };

  return (
    <div className="flex flex-col gap-1">
      <div className="text-white/40 text-[10px] tracking-widest">{label}</div>
      <svg width={width} height={height} className="overflow-visible">
        <motion.polyline
          points={points}
          fill="none"
          strokeWidth="2"
          className={colorClasses[color]}
          initial={{ pathLength: 0 }}
          animate={{ pathLength: 1 }}
          transition={{ duration: 0.5 }}
        />
        <motion.polygon
          points={`${points} ${width},${height} 0,${height}`}
          className={colorClasses[color]}
          fillOpacity="0.1"
          strokeWidth="0"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          transition={{ duration: 0.5 }}
        />
      </svg>
    </div>
  );
}
