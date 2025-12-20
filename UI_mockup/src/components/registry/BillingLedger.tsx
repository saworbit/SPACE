import { useState } from 'react';
import {
  BarChart,
  Bar,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  Legend,
  ResponsiveContainer,
} from 'recharts';
import { Download, TrendingUp } from 'lucide-react';

interface UsageData {
  date: string;
  storage: number;
  compute: number;
  network: number;
}

export function BillingLedger() {
  const [timeRange, setTimeRange] = useState<'7d' | '30d' | '90d'>('30d');

  const usageData: UsageData[] = [
    { date: '2024-11-01', storage: 2400, compute: 1200, network: 800 },
    { date: '2024-11-02', storage: 2380, compute: 1350, network: 920 },
    { date: '2024-11-03', storage: 2420, compute: 1180, network: 750 },
    { date: '2024-11-04', storage: 2450, compute: 1420, network: 1100 },
    { date: '2024-11-05', storage: 2490, compute: 1280, network: 880 },
    { date: '2024-11-06', storage: 2510, compute: 1390, network: 950 },
    { date: '2024-11-07', storage: 2530, compute: 1310, network: 820 },
    { date: '2024-11-08', storage: 2560, compute: 1450, network: 1050 },
    { date: '2024-11-09', storage: 2580, compute: 1220, network: 780 },
    { date: '2024-11-10', storage: 2600, compute: 1380, network: 910 },
    { date: '2024-11-11', storage: 2620, compute: 1290, network: 840 },
    { date: '2024-11-12', storage: 2650, compute: 1410, network: 990 },
    { date: '2024-11-13', storage: 2670, compute: 1330, network: 870 },
  ];

  const totalStorage = usageData.reduce((sum, d) => sum + d.storage, 0);
  const totalCompute = usageData.reduce((sum, d) => sum + d.compute, 0);
  const totalNetwork = usageData.reduce((sum, d) => sum + d.network, 0);

  const avgStorage = Math.round(totalStorage / usageData.length);
  const avgCompute = Math.round(totalCompute / usageData.length);
  const avgNetwork = Math.round(totalNetwork / usageData.length);

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="bg-white border-b border-gray-200 px-6 py-4 flex items-center justify-between flex-shrink-0">
        <div>
          <h2 className="text-gray-900 tracking-tight">Billing & Usage Ledger</h2>
          <p className="text-gray-500 text-sm mt-1">Resource consumption and chargeback metrics</p>
        </div>
        <div className="flex gap-2">
          <button className="px-4 py-2 text-sm border border-gray-300 bg-white text-gray-700 hover:bg-gray-50 flex items-center gap-2">
            <Download className="w-4 h-4" />
            Export PDF
          </button>
        </div>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-auto bg-gray-50 p-6">
        {/* Time Range Selector */}
        <div className="flex gap-2 mb-6">
          {(['7d', '30d', '90d'] as const).map(range => (
            <button
              key={range}
              onClick={() => setTimeRange(range)}
              className={`px-4 py-2 text-sm border transition-colors ${
                timeRange === range
                  ? 'bg-gray-900 text-white border-gray-900'
                  : 'bg-white text-gray-700 border-gray-300 hover:bg-gray-50'
              }`}
            >
              Last {range}
            </button>
          ))}
        </div>

        {/* Summary Cards */}
        <div className="grid grid-cols-3 gap-6 mb-6">
          <div className="bg-white border border-gray-200 p-6">
            <div className="flex items-center justify-between mb-2">
              <div className="text-xs text-gray-500 uppercase tracking-wider">Storage Used</div>
              <TrendingUp className="w-4 h-4 text-green-600" />
            </div>
            <div className="text-2xl text-gray-900 mb-1">{(avgStorage / 1000).toFixed(2)} TB</div>
            <div className="text-xs text-gray-500">Avg per day</div>
            <div className="mt-4 pt-4 border-t border-gray-200">
              <div className="text-xs text-gray-500">Total (GB-Hours)</div>
              <div className="text-sm text-gray-900 font-mono mt-1">
                {totalStorage.toLocaleString()}
              </div>
            </div>
          </div>

          <div className="bg-white border border-gray-200 p-6">
            <div className="flex items-center justify-between mb-2">
              <div className="text-xs text-gray-500 uppercase tracking-wider">Compute Used</div>
              <TrendingUp className="w-4 h-4 text-blue-600" />
            </div>
            <div className="text-2xl text-gray-900 mb-1">{(avgCompute / 1000).toFixed(2)}B</div>
            <div className="text-xs text-gray-500">Avg instructions/day</div>
            <div className="mt-4 pt-4 border-t border-gray-200">
              <div className="text-xs text-gray-500">Total Instructions</div>
              <div className="text-sm text-gray-900 font-mono mt-1">
                {(totalCompute * 1000000).toLocaleString()}
              </div>
            </div>
          </div>

          <div className="bg-white border border-gray-200 p-6">
            <div className="flex items-center justify-between mb-2">
              <div className="text-xs text-gray-500 uppercase tracking-wider">Network Egress</div>
              <TrendingUp className="w-4 h-4 text-purple-600" />
            </div>
            <div className="text-2xl text-gray-900 mb-1">{(avgNetwork / 1000).toFixed(2)} TB</div>
            <div className="text-xs text-gray-500">Avg per day</div>
            <div className="mt-4 pt-4 border-t border-gray-200">
              <div className="text-xs text-gray-500">Total Transferred</div>
              <div className="text-sm text-gray-900 font-mono mt-1">
                {(totalNetwork / 1000).toFixed(2)} TB
              </div>
            </div>
          </div>
        </div>

        {/* Chart */}
        <div className="bg-white border border-gray-200 p-6">
          <h3 className="text-gray-900 mb-4">Usage Over Time</h3>
          <ResponsiveContainer width="100%" height={300}>
            <BarChart data={usageData}>
              <CartesianGrid strokeDasharray="3 3" stroke="#e5e7eb" />
              <XAxis
                dataKey="date"
                tick={{ fontSize: 12, fill: '#6b7280' }}
                tickFormatter={value =>
                  new Date(value).toLocaleDateString('en-US', { month: 'short', day: 'numeric' })
                }
              />
              <YAxis tick={{ fontSize: 12, fill: '#6b7280' }} />
              <Tooltip
                contentStyle={{
                  backgroundColor: 'white',
                  border: '1px solid #e5e7eb',
                  fontSize: '12px',
                }}
              />
              <Legend wrapperStyle={{ fontSize: '12px' }} />
              <Bar dataKey="storage" fill="#10b981" name="Storage (GB)" />
              <Bar dataKey="compute" fill="#3b82f6" name="Compute (M inst)" />
              <Bar dataKey="network" fill="#8b5cf6" name="Network (GB)" />
            </BarChart>
          </ResponsiveContainer>
        </div>

        {/* Detailed Table */}
        <div className="mt-6 bg-white border border-gray-200">
          <div className="px-6 py-4 border-b border-gray-200">
            <h3 className="text-gray-900">Detailed Breakdown</h3>
          </div>
          <table className="w-full">
            <thead>
              <tr className="bg-gray-50 border-b border-gray-200">
                <th className="px-4 py-3 text-left text-xs text-gray-700 tracking-wider uppercase">
                  Date
                </th>
                <th className="px-4 py-3 text-right text-xs text-gray-700 tracking-wider uppercase">
                  Storage (GB-hrs)
                </th>
                <th className="px-4 py-3 text-right text-xs text-gray-700 tracking-wider uppercase">
                  Compute (M inst)
                </th>
                <th className="px-4 py-3 text-right text-xs text-gray-700 tracking-wider uppercase">
                  Network (GB)
                </th>
                <th className="px-4 py-3 text-right text-xs text-gray-700 tracking-wider uppercase">
                  Total Cost
                </th>
              </tr>
            </thead>
            <tbody className="divide-y divide-gray-200">
              {usageData.map(data => {
                const cost = (
                  data.storage * 0.023 +
                  data.compute * 0.015 +
                  data.network * 0.09
                ).toFixed(2);
                return (
                  <tr key={data.date} className="hover:bg-gray-50 h-[30px]">
                    <td className="px-4 py-2 text-sm text-gray-900 font-mono">
                      {new Date(data.date).toLocaleDateString('en-US', {
                        year: 'numeric',
                        month: 'short',
                        day: 'numeric',
                      })}
                    </td>
                    <td className="px-4 py-2 text-sm text-gray-600 font-mono text-right">
                      {data.storage.toLocaleString()}
                    </td>
                    <td className="px-4 py-2 text-sm text-gray-600 font-mono text-right">
                      {data.compute.toLocaleString()}
                    </td>
                    <td className="px-4 py-2 text-sm text-gray-600 font-mono text-right">
                      {data.network.toLocaleString()}
                    </td>
                    <td className="px-4 py-2 text-sm text-gray-900 font-mono text-right">
                      ${cost}
                    </td>
                  </tr>
                );
              })}
              <tr className="bg-gray-50 border-t-2 border-gray-300">
                <td className="px-4 py-3 text-sm text-gray-900">Total</td>
                <td className="px-4 py-3 text-sm text-gray-900 font-mono text-right">
                  {totalStorage.toLocaleString()}
                </td>
                <td className="px-4 py-3 text-sm text-gray-900 font-mono text-right">
                  {totalCompute.toLocaleString()}
                </td>
                <td className="px-4 py-3 text-sm text-gray-900 font-mono text-right">
                  {totalNetwork.toLocaleString()}
                </td>
                <td className="px-4 py-3 text-sm text-gray-900 font-mono text-right">
                  ${(totalStorage * 0.023 + totalCompute * 0.015 + totalNetwork * 0.09).toFixed(2)}
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>
    </div>
  );
}
