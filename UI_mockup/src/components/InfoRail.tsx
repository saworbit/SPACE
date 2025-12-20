import { useEffect, useState } from 'react';
import { Activity, Zap, Network } from 'lucide-react';

export function InfoRail() {
  const [metrics, setMetrics] = useState({
    iops: 0,
    latency: 0,
    federationHealth: 100,
  });

  useEffect(() => {
    // Simulate real-time metrics
    const interval = setInterval(() => {
      setMetrics({
        iops: Math.floor(50000 + Math.random() * 20000),
        latency: parseFloat((0.5 + Math.random() * 0.3).toFixed(2)),
        federationHealth: Math.floor(95 + Math.random() * 5),
      });
    }, 1000);

    return () => clearInterval(interval);
  }, []);

  return (
    <div className="absolute top-0 left-0 right-0 h-12 bg-glass border-b border-glass flex items-center justify-between px-6 z-40">
      <div className="flex items-center gap-2">
        <div className="w-2 h-2 rounded-full bg-neon-cyan animate-pulse"></div>
        <span className="text-neon-cyan text-sm tracking-wider">ORBIT COMMAND v1.0</span>
      </div>

      <div className="flex items-center gap-8">
        <Ticker
          icon={<Zap className="w-4 h-4" />}
          label="IOPS"
          value={metrics.iops.toLocaleString()}
          color="cyan"
        />
        <Ticker
          icon={<Activity className="w-4 h-4" />}
          label="LATENCY"
          value={`${metrics.latency}ms`}
          color="purple"
        />
        <Ticker
          icon={<Network className="w-4 h-4" />}
          label="FEDERATION"
          value={`${metrics.federationHealth}%`}
          color={metrics.federationHealth > 98 ? 'cyan' : 'orange'}
        />
      </div>
    </div>
  );
}

function Ticker({
  icon,
  label,
  value,
  color,
}: {
  icon: React.ReactNode;
  label: string;
  value: string;
  color: 'cyan' | 'purple' | 'orange';
}) {
  const colorClasses = {
    cyan: 'text-neon-cyan',
    purple: 'text-nebula-purple',
    orange: 'text-supernova-orange',
  };

  return (
    <div className="flex items-center gap-2">
      <div className={colorClasses[color]}>{icon}</div>
      <div className="flex flex-col">
        <span className="text-white/40 text-[10px] tracking-widest">{label}</span>
        <span className={`${colorClasses[color]} text-sm tabular-nums`}>{value}</span>
      </div>
    </div>
  );
}
