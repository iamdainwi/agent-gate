'use client'

import { useEffect, useState } from 'react'
import { BarChart, Bar, XAxis, YAxis, Tooltip, ResponsiveContainer } from 'recharts'
import { getToolStats } from '@/lib/api'
import type { ToolStat } from '@/lib/types'

export default function AnalyticsPage() {
  const [stats, setStats] = useState<ToolStat[]>([])
  const [loading, setLoading] = useState(true)

  useEffect(() => {
    getToolStats()
      .then(setStats)
      .catch(console.error)
      .finally(() => setLoading(false))
  }, [])

  const top10 = [...stats]
    .sort((a, b) => b.total_calls - a.total_calls)
    .slice(0, 10)

  return (
    <div className="space-y-6">
      <h1 className="text-2xl font-bold text-white">Analytics</h1>

      {loading && <p className="text-gray-500 text-sm">Loading…</p>}

      {!loading && top10.length > 0 && (
        <div className="bg-gray-900 rounded-xl p-5 border border-gray-800">
          <h2 className="text-sm font-medium text-gray-400 mb-4">Top tools by call count</h2>
          <ResponsiveContainer width="100%" height={220}>
            <BarChart data={top10} layout="vertical" margin={{ top: 0, right: 20, left: 10, bottom: 0 }}>
              <XAxis type="number" tick={{ fill: '#6b7280', fontSize: 11 }} />
              <YAxis
                type="category"
                dataKey="tool_name"
                tick={{ fill: '#a5b4fc', fontSize: 11, fontFamily: 'monospace' }}
                width={140}
              />
              <Tooltip
                contentStyle={{ background: '#111827', border: '1px solid #374151', color: '#f3f4f6' }}
              />
              <Bar dataKey="total_calls" fill="#6366f1" radius={[0, 3, 3, 0]} />
            </BarChart>
          </ResponsiveContainer>
        </div>
      )}

      {!loading && stats.length > 0 && (
        <div className="overflow-x-auto rounded-lg border border-gray-800">
          <table className="w-full text-sm text-left">
            <thead className="bg-gray-900 text-gray-400 text-xs uppercase tracking-wider">
              <tr>
                <th className="px-4 py-3">Tool</th>
                <th className="px-4 py-3">Total Calls</th>
                <th className="px-4 py-3">Errors</th>
                <th className="px-4 py-3">Denials</th>
                <th className="px-4 py-3">Avg Latency (ms)</th>
                <th className="px-4 py-3">Last Seen</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-gray-800">
              {stats.map((s) => (
                <tr key={s.tool_name} className="bg-gray-950 hover:bg-gray-900 transition-colors">
                  <td className="px-4 py-3 font-mono text-indigo-300">{s.tool_name}</td>
                  <td className="px-4 py-3 text-gray-300">{s.total_calls}</td>
                  <td className="px-4 py-3 text-yellow-400">{s.error_count}</td>
                  <td className="px-4 py-3 text-red-400">{s.denial_count}</td>
                  <td className="px-4 py-3 text-gray-300">
                    {s.avg_latency_ms != null ? s.avg_latency_ms.toFixed(1) : '—'}
                  </td>
                  <td className="px-4 py-3 text-gray-400 whitespace-nowrap">
                    {new Date(s.last_seen).toLocaleString()}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {!loading && stats.length === 0 && (
        <p className="text-gray-500 text-sm">No tool statistics available.</p>
      )}
    </div>
  )
}
