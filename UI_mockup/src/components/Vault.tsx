import { useState, useEffect } from 'react';
import { motion } from 'motion/react';
import { Shield, Key, Clock, Lock, AlertTriangle, Activity } from 'lucide-react';

interface AuditEntry {
  id: string;
  timestamp: Date;
  action: string;
  user: string;
  resource: string;
  status: 'success' | 'warning' | 'error';
}

export function Vault() {
  const [keyTTL, setKeyTTL] = useState(3600);
  const [auditLog, setAuditLog] = useState<AuditEntry[]>([]);
  const [policy, setPolicy] = useState(`# Phase4 Security Policy
version: "1.0"
encryption:
  algorithm: kyber-1024
  rotation_interval: 3600s
  
access_control:
  - role: admin
    permissions: ["read", "write", "delete"]
  - role: operator
    permissions: ["read", "write"]
  - role: viewer
    permissions: ["read"]
    
audit:
  retention: 90d
  immutable: true`);

  useEffect(() => {
    // Simulate key TTL countdown
    const interval = setInterval(() => {
      setKeyTTL(prev => {
        if (prev <= 0) return 3600;
        return prev - 1;
      });
    }, 1000);

    // Generate initial audit log
    const entries: AuditEntry[] = [];
    const actions = ['ACCESS', 'MODIFY', 'DELETE', 'CREATE', 'ROTATE_KEY'];
    const users = ['admin@orbit', 'operator@orbit', 'system'];
    const resources = ['capsule-001', 'node-alpha', 'encryption-key', 'namespace-prod'];
    
    for (let i = 0; i < 10; i++) {
      entries.push({
        id: `audit-${i}`,
        timestamp: new Date(Date.now() - i * 60000),
        action: actions[Math.floor(Math.random() * actions.length)],
        user: users[Math.floor(Math.random() * users.length)],
        resource: resources[Math.floor(Math.random() * resources.length)],
        status: Math.random() > 0.9 ? 'warning' : 'success'
      });
    }
    setAuditLog(entries);

    // Add new entries periodically
    const auditInterval = setInterval(() => {
      const newEntry: AuditEntry = {
        id: `audit-${Date.now()}`,
        timestamp: new Date(),
        action: actions[Math.floor(Math.random() * actions.length)],
        user: users[Math.floor(Math.random() * users.length)],
        resource: resources[Math.floor(Math.random() * resources.length)],
        status: Math.random() > 0.9 ? 'warning' : 'success'
      };
      setAuditLog(prev => [newEntry, ...prev.slice(0, 19)]);
    }, 5000);

    return () => {
      clearInterval(interval);
      clearInterval(auditInterval);
    };
  }, []);

  const formatTime = (seconds: number) => {
    const hours = Math.floor(seconds / 3600);
    const minutes = Math.floor((seconds % 3600) / 60);
    const secs = seconds % 60;
    return `${hours.toString().padStart(2, '0')}:${minutes.toString().padStart(2, '0')}:${secs.toString().padStart(2, '0')}`;
  };

  return (
    <div className="w-full h-full relative overflow-hidden">
      {/* Title */}
      <div className="absolute top-8 left-8 z-10">
        <h1 className="text-neon-cyan text-2xl tracking-wider">THE VAULT</h1>
        <p className="text-white/40 text-sm mt-1">Security Control // The Citadel</p>
      </div>

      {/* Key Rotation Display */}
      <div className="absolute top-8 right-8 z-10">
        <div className="bg-glass border border-neon-cyan rounded-lg p-6 w-64">
          <div className="flex items-center gap-2 mb-4">
            <Key className="w-5 h-5 text-neon-cyan" />
            <span className="text-neon-cyan">Kyber Key Rotation</span>
          </div>
          
          {/* Circular countdown */}
          <div className="relative w-32 h-32 mx-auto mb-4">
            <svg className="transform -rotate-90 w-32 h-32">
              <circle
                cx="64"
                cy="64"
                r="56"
                stroke="currentColor"
                strokeWidth="8"
                fill="none"
                className="text-white/10"
              />
              <motion.circle
                cx="64"
                cy="64"
                r="56"
                stroke="currentColor"
                strokeWidth="8"
                fill="none"
                className="text-neon-cyan"
                strokeDasharray={`${2 * Math.PI * 56}`}
                strokeDashoffset={`${2 * Math.PI * 56 * (1 - keyTTL / 3600)}`}
                strokeLinecap="round"
              />
            </svg>
            <div className="absolute inset-0 flex flex-col items-center justify-center">
              <Clock className="w-6 h-6 text-neon-cyan mb-1" />
              <span className="text-white text-sm font-mono">{formatTime(keyTTL)}</span>
            </div>
          </div>

          <div className="text-center text-white/40 text-xs">
            Time until next rotation
          </div>
        </div>
      </div>

      {/* Main Content - Split Screen */}
      <div className="absolute top-48 left-8 right-8 bottom-8 flex gap-6">
        {/* Left: Policy Editor */}
        <div className="flex-1 flex flex-col">
          <div className="flex items-center gap-2 mb-4">
            <Shield className="w-5 h-5 text-nebula-purple" />
            <h2 className="text-nebula-purple">Policy Editor</h2>
          </div>
          
          <div className="flex-1 bg-glass border border-glass rounded-lg p-4 overflow-hidden flex flex-col">
            <div className="flex items-center justify-between mb-3">
              <span className="text-white/60 text-sm">Visual Rule Builder</span>
              <span className="text-neon-cyan text-xs">ACTIVE</span>
            </div>

            <div className="space-y-3 mb-4">
              <PolicyRule
                icon={<Lock className="w-4 h-4" />}
                label="Encryption Algorithm"
                value="Kyber-1024"
                status="active"
              />
              <PolicyRule
                icon={<Clock className="w-4 h-4" />}
                label="Key Rotation Interval"
                value="3600s"
                status="active"
              />
              <PolicyRule
                icon={<Shield className="w-4 h-4" />}
                label="Access Control Roles"
                value="3 configured"
                status="active"
              />
              <PolicyRule
                icon={<AlertTriangle className="w-4 h-4" />}
                label="Audit Retention"
                value="90 days"
                status="warning"
              />
            </div>

            <div className="text-white/40 text-xs mb-2">YAML Representation (Read-only)</div>
            <div className="flex-1 bg-black/40 rounded p-3 overflow-auto">
              <pre className="text-white/80 text-xs font-mono whitespace-pre-wrap">
                {policy}
              </pre>
            </div>
          </div>
        </div>

        {/* Right: Audit Stream */}
        <div className="flex-1 flex flex-col">
          <div className="flex items-center gap-2 mb-4">
            <Activity className="w-5 h-5 text-neon-cyan" />
            <h2 className="text-neon-cyan">Audit Stream</h2>
            <span className="text-white/40 text-xs ml-auto">IMMUTABLE LOG</span>
          </div>

          <div className="flex-1 bg-black/60 border border-neon-cyan/20 rounded-lg overflow-hidden">
            <div className="h-full overflow-auto p-4 space-y-2 font-mono text-xs">
              {auditLog.map((entry, index) => (
                <motion.div
                  key={entry.id}
                  className="flex gap-3 text-green-400"
                  initial={{ opacity: 0, x: -20 }}
                  animate={{ opacity: 1, x: 0 }}
                  transition={{ delay: index * 0.05 }}
                >
                  <span className="text-white/40">
                    {entry.timestamp.toLocaleTimeString()}
                  </span>
                  <span className={
                    entry.status === 'success' ? 'text-neon-cyan' :
                    entry.status === 'warning' ? 'text-supernova-orange' :
                    'text-red-500'
                  }>
                    [{entry.status.toUpperCase()}]
                  </span>
                  <span className="text-nebula-purple">{entry.user}</span>
                  <span className="text-white">{entry.action}</span>
                  <span className="text-white/60">{entry.resource}</span>
                </motion.div>
              ))}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

function PolicyRule({ 
  icon, 
  label, 
  value, 
  status 
}: { 
  icon: React.ReactNode; 
  label: string; 
  value: string; 
  status: 'active' | 'warning';
}) {
  return (
    <div className="bg-black/20 border border-white/10 rounded p-3 flex items-center gap-3">
      <div className={status === 'active' ? 'text-neon-cyan' : 'text-supernova-orange'}>
        {icon}
      </div>
      <div className="flex-1">
        <div className="text-white text-sm">{label}</div>
        <div className="text-white/60 text-xs">{value}</div>
      </div>
      <div className={`w-2 h-2 rounded-full ${
        status === 'active' ? 'bg-neon-cyan' : 'bg-supernova-orange'
      } animate-pulse`} />
    </div>
  );
}