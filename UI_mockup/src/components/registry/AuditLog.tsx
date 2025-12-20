import { useState, useEffect } from 'react';
import { Search, Filter, Download, Clock } from 'lucide-react';

interface AuditEntry {
  id: string;
  timestamp: string;
  principal: string;
  action: string;
  status: 'SUCCESS' | 'DENIED';
  metadata: object;
}

export function AuditLog() {
  const [entries, setEntries] = useState<AuditEntry[]>([]);
  const [searchQuery, setSearchQuery] = useState('');
  const [timeRange, setTimeRange] = useState(100); // percentage of total time

  useEffect(() => {
    // Generate mock audit entries
    const actions = [
      'CAPSULE_UPDATE',
      'ZONE_RESET',
      'KEY_ROTATE',
      'POLICY_CHANGE',
      'NODE_RESTART',
      'BACKUP_INIT',
    ];
    const principals = [
      '0x7a9f8e1b',
      '0x4c3d2a5f',
      '0x9b6e4f2c',
      'alice@orbit.sys',
      'cicd-bot@orbit.sys',
    ];

    const generateEntries: AuditEntry[] = [];
    const now = Date.now();

    for (let i = 0; i < 100; i++) {
      const timestamp = new Date(now - i * 60000);
      generateEntries.push({
        id: `audit-${i}`,
        timestamp: timestamp.toISOString(),
        principal: principals[Math.floor(Math.random() * principals.length)],
        action: actions[Math.floor(Math.random() * actions.length)],
        status: Math.random() > 0.1 ? 'SUCCESS' : 'DENIED',
        metadata: {
          ip: `10.0.${Math.floor(Math.random() * 255)}.${Math.floor(Math.random() * 255)}`,
          duration_ms: Math.floor(Math.random() * 500),
          resource_id: `res-${Math.random().toString(36).substr(2, 9)}`,
        },
      });
    }

    setEntries(generateEntries);
  }, []);

  const filteredEntries = entries
    .filter(entry => {
      if (!searchQuery) return true;
      const query = searchQuery.toLowerCase();
      return (
        entry.action.toLowerCase().includes(query) ||
        entry.principal.toLowerCase().includes(query) ||
        entry.status.toLowerCase().includes(query)
      );
    })
    .filter((entry, index) => {
      // Time travel filter
      const maxIndex = Math.floor((entries.length * timeRange) / 100);
      return index < maxIndex;
    });

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="bg-white border-b border-gray-200 px-6 py-4 flex-shrink-0">
        <div className="flex items-center justify-between mb-4">
          <div>
            <h2 className="text-gray-900 tracking-tight">Global Audit Log</h2>
            <p className="text-gray-500 text-sm mt-1">Immutable record of all API calls</p>
          </div>
          <button className="px-4 py-2 text-sm border border-gray-300 bg-white text-gray-700 hover:bg-gray-50 flex items-center gap-2">
            <Download className="w-4 h-4" />
            Export Log
          </button>
        </div>

        {/* Query Builder */}
        <div className="flex gap-3">
          <div className="flex-1 relative">
            <Search className="w-4 h-4 absolute left-3 top-1/2 -translate-y-1/2 text-gray-400" />
            <input
              type="text"
              value={searchQuery}
              onChange={e => setSearchQuery(e.target.value)}
              placeholder="severity:WARN AND service:raft-rs AND time > 10m"
              className="w-full pl-10 pr-4 py-2 border border-gray-300 focus:outline-none focus:ring-2 focus:ring-gray-900 focus:border-transparent text-sm font-mono"
            />
          </div>
          <button className="px-4 py-2 text-sm border border-gray-300 bg-white text-gray-700 hover:bg-gray-50 flex items-center gap-2">
            <Filter className="w-4 h-4" />
            Advanced Filters
          </button>
        </div>

        {/* Time Travel Slider */}
        <div className="mt-4 flex items-center gap-4">
          <Clock className="w-4 h-4 text-gray-500" />
          <span className="text-xs text-gray-600 w-24">Time Travel:</span>
          <input
            type="range"
            min="0"
            max="100"
            value={timeRange}
            onChange={e => setTimeRange(Number(e.target.value))}
            className="flex-1"
          />
          <span className="text-xs text-gray-600 font-mono w-32">
            Showing {filteredEntries.length} / {entries.length} entries
          </span>
        </div>
      </div>

      {/* Table */}
      <div className="flex-1 overflow-auto bg-gray-50">
        <div className="p-6">
          <div className="bg-white border border-gray-200">
            <table className="w-full">
              <thead className="sticky top-0 z-10">
                <tr className="bg-gray-50 border-b border-gray-200">
                  <th className="px-4 py-3 text-left text-xs text-gray-700 tracking-wider uppercase">
                    Timestamp
                  </th>
                  <th className="px-4 py-3 text-left text-xs text-gray-700 tracking-wider uppercase">
                    Principal
                  </th>
                  <th className="px-4 py-3 text-left text-xs text-gray-700 tracking-wider uppercase">
                    Action
                  </th>
                  <th className="px-4 py-3 text-left text-xs text-gray-700 tracking-wider uppercase">
                    Status
                  </th>
                  <th className="px-4 py-3 text-left text-xs text-gray-700 tracking-wider uppercase">
                    Metadata
                  </th>
                </tr>
              </thead>
              <tbody className="divide-y divide-gray-200">
                {filteredEntries.map(entry => (
                  <tr key={entry.id} className="hover:bg-gray-50 h-[30px]">
                    <td className="px-4 py-2 text-xs text-gray-600 font-mono whitespace-nowrap">
                      {new Date(entry.timestamp).toISOString().replace('T', ' ').substr(0, 23)}
                    </td>
                    <td className="px-4 py-2 text-xs text-gray-900 font-mono">{entry.principal}</td>
                    <td className="px-4 py-2 text-xs text-gray-900 font-mono">{entry.action}</td>
                    <td className="px-4 py-2 text-xs">
                      <span className="flex items-center gap-2">
                        <span
                          className={`w-2 h-2 rounded-full ${
                            entry.status === 'SUCCESS' ? 'bg-green-500' : 'bg-red-500'
                          }`}
                        />
                        <span
                          className={entry.status === 'SUCCESS' ? 'text-green-700' : 'text-red-700'}
                        >
                          {entry.status}
                        </span>
                      </span>
                    </td>
                    <td className="px-4 py-2 text-xs text-gray-600 font-mono">
                      <details className="cursor-pointer">
                        <summary className="text-gray-500 hover:text-gray-700">View JSON</summary>
                        <pre className="mt-2 p-2 bg-gray-50 border border-gray-200 rounded text-[10px] overflow-auto max-w-md">
                          {JSON.stringify(entry.metadata, null, 2)}
                        </pre>
                      </details>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>

          {filteredEntries.length === 0 && (
            <div className="text-center py-12 text-gray-500">
              No audit entries found matching your criteria
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
