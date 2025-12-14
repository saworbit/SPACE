import { useState, useEffect } from 'react';
import { motion } from 'motion/react';
import { Thermometer, Activity } from 'lucide-react';

interface Zone {
  id: number;
  status: 'sequential' | 'cold' | 'gc-pressure';
  wear: number;
  lbaRange: string;
  writePointer: number;
}

export function Foundry() {
  const [viewMode, setViewMode] = useState<'status' | 'wear'>('status');
  const [selectedZone, setSelectedZone] = useState<Zone | null>(null);
  const [zones, setZones] = useState<Zone[]>([]);

  useEffect(() => {
    // Generate zones
    const generatedZones: Zone[] = [];
    for (let i = 0; i < 400; i++) {
      const rand = Math.random();
      generatedZones.push({
        id: i,
        status: rand > 0.7 ? 'sequential' : rand > 0.4 ? 'cold' : 'gc-pressure',
        wear: Math.random() * 100,
        lbaRange: `0x${(i * 1024).toString(16).toUpperCase()}-0x${((i + 1) * 1024 - 1).toString(16).toUpperCase()}`,
        writePointer: Math.floor(Math.random() * 1024)
      });
    }
    setZones(generatedZones);
  }, []);

  const getZoneColor = (zone: Zone) => {
    if (viewMode === 'wear') {
      const wearLevel = zone.wear;
      if (wearLevel < 30) return 'bg-green-500';
      if (wearLevel < 60) return 'bg-yellow-500';
      if (wearLevel < 80) return 'bg-orange-500';
      return 'bg-red-500';
    } else {
      switch (zone.status) {
        case 'sequential': return 'bg-blue-500';
        case 'cold': return 'bg-nebula-purple';
        case 'gc-pressure': return 'bg-supernova-orange';
        default: return 'bg-white/10';
      }
    }
  };

  return (
    <div className="w-full h-full relative overflow-auto">
      {/* Title */}
      <div className="absolute top-8 left-8 z-10">
        <h1 className="text-neon-cyan text-2xl tracking-wider">THE FOUNDRY</h1>
        <p className="text-white/40 text-sm mt-1">Storage Physics // The Silicon Die</p>
      </div>

      {/* View Mode Toggle */}
      <div className="absolute top-8 right-8 z-10 flex gap-2">
        <button
          onClick={() => setViewMode('status')}
          className={`px-4 py-2 rounded border transition-all ${
            viewMode === 'status'
              ? 'bg-neon-cyan/20 border-neon-cyan text-neon-cyan'
              : 'bg-glass border-white/20 text-white/60 hover:border-white/40'
          }`}
        >
          Status View
        </button>
        <button
          onClick={() => setViewMode('wear')}
          className={`px-4 py-2 rounded border transition-all ${
            viewMode === 'wear'
              ? 'bg-neon-cyan/20 border-neon-cyan text-neon-cyan'
              : 'bg-glass border-white/20 text-white/60 hover:border-white/40'
          }`}
        >
          Wear Leveling
        </button>
      </div>

      {/* Legend */}
      <div className="absolute top-24 right-8 z-10 bg-glass border border-glass rounded p-4">
        <div className="text-white text-sm mb-3">
          {viewMode === 'status' ? 'Zone Status' : 'Wear Level'}
        </div>
        {viewMode === 'status' ? (
          <div className="space-y-2">
            <div className="flex items-center gap-2">
              <div className="w-4 h-4 bg-blue-500 rounded"></div>
              <span className="text-white/80 text-xs">Sequential Write</span>
            </div>
            <div className="flex items-center gap-2">
              <div className="w-4 h-4 bg-nebula-purple rounded"></div>
              <span className="text-white/80 text-xs">Cold Data</span>
            </div>
            <div className="flex items-center gap-2">
              <div className="w-4 h-4 bg-supernova-orange rounded"></div>
              <span className="text-white/80 text-xs">GC Pressure</span>
            </div>
          </div>
        ) : (
          <div className="space-y-2">
            <div className="flex items-center gap-2">
              <div className="w-4 h-4 bg-green-500 rounded"></div>
              <span className="text-white/80 text-xs">0-30% (Healthy)</span>
            </div>
            <div className="flex items-center gap-2">
              <div className="w-4 h-4 bg-yellow-500 rounded"></div>
              <span className="text-white/80 text-xs">30-60% (Normal)</span>
            </div>
            <div className="flex items-center gap-2">
              <div className="w-4 h-4 bg-orange-500 rounded"></div>
              <span className="text-white/80 text-xs">60-80% (Elevated)</span>
            </div>
            <div className="flex items-center gap-2">
              <div className="w-4 h-4 bg-red-500 rounded"></div>
              <span className="text-white/80 text-xs">80-100% (Critical)</span>
            </div>
          </div>
        )}
      </div>

      {/* ZNS Heatmap */}
      <div className="absolute top-48 left-8 right-8 bottom-8">
        <div className="grid grid-cols-20 gap-1 w-fit">
          {zones.map((zone) => (
            <motion.button
              key={zone.id}
              className={`w-6 h-6 rounded-sm ${getZoneColor(zone)} transition-all hover:ring-2 hover:ring-neon-cyan cursor-pointer`}
              onClick={() => setSelectedZone(zone)}
              whileHover={{ scale: 1.2 }}
              whileTap={{ scale: 0.9 }}
              initial={{ opacity: 0, scale: 0 }}
              animate={{ opacity: 1, scale: 1 }}
              transition={{ delay: zone.id * 0.001 }}
            />
          ))}
        </div>

        {/* Zone Details Panel */}
        {selectedZone && (
          <motion.div
            className="fixed top-1/2 left-1/2 -translate-x-1/2 -translate-y-1/2 bg-glass border border-neon-cyan rounded-lg p-6 z-20 min-w-96"
            initial={{ opacity: 0, scale: 0.9 }}
            animate={{ opacity: 1, scale: 1 }}
            onClick={() => setSelectedZone(null)}
          >
            <div className="flex items-center justify-between mb-4">
              <h3 className="text-neon-cyan">Zone {selectedZone.id}</h3>
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  setSelectedZone(null);
                }}
                className="text-white/60 hover:text-white"
              >
                ✕
              </button>
            </div>

            <div className="space-y-4">
              <div>
                <div className="text-white/40 text-xs mb-1">STATUS</div>
                <div className={`inline-flex items-center gap-2 px-3 py-1 rounded ${
                  selectedZone.status === 'sequential' ? 'bg-blue-500/20 text-blue-400' :
                  selectedZone.status === 'cold' ? 'bg-nebula-purple/20 text-nebula-purple' :
                  'bg-supernova-orange/20 text-supernova-orange'
                }`}>
                  {selectedZone.status === 'sequential' && <Activity className="w-4 h-4" />}
                  {selectedZone.status === 'gc-pressure' && <Thermometer className="w-4 h-4" />}
                  <span className="text-sm">{selectedZone.status.toUpperCase().replace('-', ' ')}</span>
                </div>
              </div>

              <div>
                <div className="text-white/40 text-xs mb-1">LBA RANGE</div>
                <div className="text-white font-mono text-sm">{selectedZone.lbaRange}</div>
              </div>

              <div>
                <div className="text-white/40 text-xs mb-1">WRITE POINTER</div>
                <div className="text-white font-mono text-sm">0x{selectedZone.writePointer.toString(16).toUpperCase()}</div>
              </div>

              <div>
                <div className="text-white/40 text-xs mb-1">WEAR LEVEL</div>
                <div className="flex items-center gap-3">
                  <div className="flex-1 h-2 bg-white/10 rounded-full overflow-hidden">
                    <motion.div
                      className={`h-full ${
                        selectedZone.wear < 30 ? 'bg-green-500' :
                        selectedZone.wear < 60 ? 'bg-yellow-500' :
                        selectedZone.wear < 80 ? 'bg-orange-500' :
                        'bg-red-500'
                      }`}
                      initial={{ width: 0 }}
                      animate={{ width: `${selectedZone.wear}%` }}
                      transition={{ duration: 0.5 }}
                    />
                  </div>
                  <span className="text-white text-sm w-12">{selectedZone.wear.toFixed(1)}%</span>
                </div>
              </div>
            </div>
          </motion.div>
        )}
      </div>
    </div>
  );
}
