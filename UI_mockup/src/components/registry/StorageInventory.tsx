import { useState } from 'react';
import { HardDrive, AlertTriangle, Calendar } from 'lucide-react';

interface Drive {
  id: string;
  serialNumber: string;
  model: string;
  firmware: string;
  powerOnHours: number;
  wearIndicator: number;
  reallocatedSectors: number;
  crcErrors: number;
  status: 'healthy' | 'warning' | 'critical';
  lastMaintenance: string;
}

export function StorageInventory() {
  const [selectedDrive, setSelectedDrive] = useState<Drive | null>(null);

  const drives: Drive[] = [
    {
      id: 'nvme0',
      serialNumber: 'S5GVNX0R123456',
      model: 'Samsung PM9A3 3.84TB',
      firmware: 'GDC7302Q',
      powerOnHours: 12847,
      wearIndicator: 98,
      reallocatedSectors: 0,
      crcErrors: 2,
      status: 'healthy',
      lastMaintenance: '2024-11-01'
    },
    {
      id: 'nvme1',
      serialNumber: 'S5GVNX0R123457',
      model: 'Samsung PM9A3 3.84TB',
      firmware: 'GDC7302Q',
      powerOnHours: 11203,
      wearIndicator: 95,
      reallocatedSectors: 0,
      crcErrors: 0,
      status: 'healthy',
      lastMaintenance: '2024-11-01'
    },
    {
      id: 'nvme2',
      serialNumber: 'S5GVNX0R123458',
      model: 'Samsung PM9A3 3.84TB',
      firmware: 'GDC7301Q',
      powerOnHours: 18392,
      wearIndicator: 72,
      reallocatedSectors: 3,
      crcErrors: 15,
      status: 'warning',
      lastMaintenance: '2024-09-15'
    },
    {
      id: 'nvme3',
      serialNumber: 'S5GVNX0R123459',
      model: 'Samsung PM9A3 3.84TB',
      firmware: 'GDC7302Q',
      powerOnHours: 9847,
      wearIndicator: 99,
      reallocatedSectors: 0,
      crcErrors: 1,
      status: 'healthy',
      lastMaintenance: '2024-11-01'
    },
    {
      id: 'nvme4',
      serialNumber: 'S5GVNX0R123460',
      model: 'Samsung PM9A3 3.84TB',
      firmware: 'GDC7301Q',
      powerOnHours: 21384,
      wearIndicator: 45,
      reallocatedSectors: 87,
      crcErrors: 234,
      status: 'critical',
      lastMaintenance: '2024-08-01'
    }
  ];

  const getStatusColor = (status: string) => {
    switch (status) {
      case 'healthy': return 'text-green-600 bg-green-50';
      case 'warning': return 'text-yellow-600 bg-yellow-50';
      case 'critical': return 'text-red-600 bg-red-50';
      default: return 'text-gray-600 bg-gray-50';
    }
  };

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="bg-white border-b border-gray-200 px-6 py-4 flex items-center justify-between flex-shrink-0">
        <div>
          <h2 className="text-gray-900 tracking-tight">Storage Inventory</h2>
          <p className="text-gray-500 text-sm mt-1">NVMe drive health and maintenance</p>
        </div>
        <button className="px-4 py-2 text-sm border border-gray-300 bg-white text-gray-700 hover:bg-gray-50 flex items-center gap-2">
          <Calendar className="w-4 h-4" />
          Schedule Maintenance
        </button>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-auto bg-gray-50">
        <div className="p-6">
          <div className="bg-white border border-gray-200">
            <table className="w-full">
              <thead>
                <tr className="bg-gray-50 border-b border-gray-200">
                  <th className="px-4 py-3 text-left text-xs text-gray-700 tracking-wider uppercase">Device</th>
                  <th className="px-4 py-3 text-left text-xs text-gray-700 tracking-wider uppercase">Serial Number</th>
                  <th className="px-4 py-3 text-left text-xs text-gray-700 tracking-wider uppercase">Model</th>
                  <th className="px-4 py-3 text-left text-xs text-gray-700 tracking-wider uppercase">Firmware</th>
                  <th className="px-4 py-3 text-left text-xs text-gray-700 tracking-wider uppercase">Power-On Hours</th>
                  <th className="px-4 py-3 text-left text-xs text-gray-700 tracking-wider uppercase">Media Wear</th>
                  <th className="px-4 py-3 text-left text-xs text-gray-700 tracking-wider uppercase">Errors</th>
                  <th className="px-4 py-3 text-left text-xs text-gray-700 tracking-wider uppercase">Status</th>
                  <th className="px-4 py-3 text-left text-xs text-gray-700 tracking-wider uppercase">Actions</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-gray-200">
                {drives.map(drive => (
                  <tr 
                    key={drive.id} 
                    className="hover:bg-gray-50 h-[30px] cursor-pointer"
                    onClick={() => setSelectedDrive(drive)}
                  >
                    <td className="px-4 py-2 text-sm text-gray-900 font-mono">
                      <div className="flex items-center gap-2">
                        <HardDrive className="w-4 h-4 text-gray-400" />
                        {drive.id}
                      </div>
                    </td>
                    <td className="px-4 py-2 text-sm text-gray-600 font-mono">
                      {drive.serialNumber}
                    </td>
                    <td className="px-4 py-2 text-sm text-gray-900">
                      {drive.model}
                    </td>
                    <td className="px-4 py-2 text-sm text-gray-600 font-mono">
                      {drive.firmware}
                    </td>
                    <td className="px-4 py-2 text-sm text-gray-600 font-mono">
                      {drive.powerOnHours.toLocaleString()}
                    </td>
                    <td className="px-4 py-2 text-sm">
                      <div className="flex items-center gap-2">
                        <div className="w-20 h-2 bg-gray-200 rounded-full overflow-hidden">
                          <div 
                            className={`h-full ${
                              drive.wearIndicator > 80 ? 'bg-green-500' :
                              drive.wearIndicator > 50 ? 'bg-yellow-500' :
                              'bg-red-500'
                            }`}
                            style={{ width: `${drive.wearIndicator}%` }}
                          />
                        </div>
                        <span className="text-gray-600 font-mono text-xs">{drive.wearIndicator}%</span>
                      </div>
                    </td>
                    <td className="px-4 py-2 text-sm">
                      <div className="text-gray-600 font-mono text-xs">
                        {drive.reallocatedSectors > 0 && (
                          <div className="text-yellow-600">RS: {drive.reallocatedSectors}</div>
                        )}
                        {drive.crcErrors > 0 && (
                          <div className="text-gray-500">CRC: {drive.crcErrors}</div>
                        )}
                        {drive.reallocatedSectors === 0 && drive.crcErrors === 0 && (
                          <div className="text-green-600">None</div>
                        )}
                      </div>
                    </td>
                    <td className="px-4 py-2 text-sm">
                      <span className={`inline-flex px-2 py-1 text-xs uppercase ${getStatusColor(drive.status)}`}>
                        {drive.status}
                      </span>
                    </td>
                    <td className="px-4 py-2 text-sm">
                      <button className="text-gray-600 hover:text-gray-900 text-xs">
                        Details
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>

          {/* Drive Details Panel */}
          {selectedDrive && (
            <div className="mt-6 bg-white border border-gray-200 p-6">
              <div className="flex items-center justify-between mb-4">
                <h3 className="text-gray-900">Drive Details: {selectedDrive.id}</h3>
                <button 
                  onClick={() => setSelectedDrive(null)}
                  className="text-gray-500 hover:text-gray-700"
                >
                  ✕
                </button>
              </div>

              <div className="grid grid-cols-3 gap-6">
                <div>
                  <div className="text-xs text-gray-500 uppercase tracking-wider mb-1">Serial Number</div>
                  <div className="text-sm text-gray-900 font-mono">{selectedDrive.serialNumber}</div>
                </div>
                <div>
                  <div className="text-xs text-gray-500 uppercase tracking-wider mb-1">Model</div>
                  <div className="text-sm text-gray-900">{selectedDrive.model}</div>
                </div>
                <div>
                  <div className="text-xs text-gray-500 uppercase tracking-wider mb-1">Firmware Version</div>
                  <div className="text-sm text-gray-900 font-mono">{selectedDrive.firmware}</div>
                </div>
                <div>
                  <div className="text-xs text-gray-500 uppercase tracking-wider mb-1">Power-On Hours</div>
                  <div className="text-sm text-gray-900 font-mono">{selectedDrive.powerOnHours.toLocaleString()} hrs</div>
                  <div className="text-xs text-gray-500">≈ {Math.round(selectedDrive.powerOnHours / 24)} days</div>
                </div>
                <div>
                  <div className="text-xs text-gray-500 uppercase tracking-wider mb-1">Media Wear Indicator</div>
                  <div className="text-sm text-gray-900 font-mono">{selectedDrive.wearIndicator}% Remaining</div>
                </div>
                <div>
                  <div className="text-xs text-gray-500 uppercase tracking-wider mb-1">Last Maintenance</div>
                  <div className="text-sm text-gray-900 font-mono">{selectedDrive.lastMaintenance}</div>
                </div>
              </div>

              <div className="mt-6 pt-6 border-t border-gray-200">
                <div className="text-xs text-gray-500 uppercase tracking-wider mb-3">Error Counters</div>
                <div className="grid grid-cols-2 gap-4">
                  <div className="flex items-center justify-between p-3 bg-gray-50 border border-gray-200">
                    <span className="text-sm text-gray-700">Reallocated Sectors</span>
                    <span className={`font-mono ${selectedDrive.reallocatedSectors > 0 ? 'text-yellow-600' : 'text-green-600'}`}>
                      {selectedDrive.reallocatedSectors}
                    </span>
                  </div>
                  <div className="flex items-center justify-between p-3 bg-gray-50 border border-gray-200">
                    <span className="text-sm text-gray-700">CRC Errors</span>
                    <span className={`font-mono ${selectedDrive.crcErrors > 10 ? 'text-yellow-600' : 'text-gray-600'}`}>
                      {selectedDrive.crcErrors}
                    </span>
                  </div>
                </div>
              </div>

              {selectedDrive.status === 'critical' && (
                <div className="mt-6 p-4 bg-red-50 border border-red-200 flex items-start gap-3">
                  <AlertTriangle className="w-5 h-5 text-red-600 flex-shrink-0 mt-0.5" />
                  <div>
                    <div className="text-sm text-red-900">Critical Drive Status</div>
                    <div className="text-xs text-red-700 mt-1">
                      This drive requires immediate attention. Schedule maintenance to prevent data loss.
                    </div>
                  </div>
                </div>
              )}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
