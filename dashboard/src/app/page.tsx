'use client'

import { useEffect, useState, useRef } from 'react'
import { BarChart, Bar, XAxis, YAxis, Tooltip, ResponsiveContainer } from 'recharts'
import { getOverviewStats, WS_BASE } from '@/lib/api'
import type { OverviewStats, InvocationRecord } from '@/lib/types'
import KpiCard from '@/components/KpiCard'
import InvocationsTable from '@/components/InvocationsTable'

export default function OverviewPage() {
  const [stats, setStats] = useState<OverviewStats | null>(null)
  const [liveRecords, setLiveRecords] = useState<InvocationRecord[]>([])
  const wsRef = useRef<WebSocket | null>(null)

  useEffect(() => {
    getOverviewStats().then(setStats).catch(console.error)
  }, [])

  useEffect(() => {
    function connect() {
      const ws = new WebSocket(`${WS_BASE}/api/ws/live`)
      wsRef.current = ws

      ws.onmessage = (e) => {
        try {
          const record: InvocationRecord = JSON.parse(e.data)
          setLiveRecords((prev) => [record, ...prev].slice(0, 20))
        } catch {
          // malformed frame — ignore
        }
      }

      ws.onclose = () => {
        setTimeout(connect, 3000)
      }
    }

    connect()
    return () => {
      wsRef.current?.close()
    }
  }, [])

  return (
    <div className="space-y-6">
      <h1 className="text-2xl font-bold text-white">Overview</h1>

      <div className="grid grid-cols-2 gap-4 lg:grid-cols-4">
        <KpiCard title="Total Calls" value={stats?.total_calls ?? '—'} />
        <KpiCard title="Total Denials" value={stats?.total_denials ?? '—'} />
        <KpiCard
          title="Avg Latency"
          value={stats?.avg_latency_ms != null ? `${stats.avg_latency_ms.toFixed(1)} ms` : '—'}
        />
        <KpiCard
          title="Calls / Min"
          value={stats?.calls_per_minute_now ?? '—'}
          subtitle="current rate"
        />
      </div>

      {stats?.sparkline && stats.sparkline.length > 0 && (
        <div className="bg-gray-900 rounded-xl p-5 border border-gray-800">
          <h2 className="text-sm font-medium text-gray-400 mb-4">Calls — last 60 minutes</h2>
          <ResponsiveContainer width="100%" height={180}>
            <BarChart data={stats.sparkline} margin={{ top: 0, right: 0, left: -20, bottom: 0 }}>
              <XAxis
                dataKey="bucket"
                tick={{ fill: '#6b7280', fontSize: 10 }}
                tickFormatter={(v) => new Date(v).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}
              />
              <YAxis tick={{ fill: '#6b7280', fontSize: 10 }} />
              <Tooltip
                contentStyle={{ background: '#111827', border: '1px solid #374151', color: '#f3f4f6' }}
                labelFormatter={(v) => new Date(v).toLocaleTimeString()}
              />
              <Bar dataKey="count" fill="#6366f1" radius={[3, 3, 0, 0]} />
            </BarChart>
          </ResponsiveContainer>
        </div>
      )}

      <div>
        <h2 className="text-lg font-semibold text-white mb-3">Live Feed</h2>
        <InvocationsTable records={liveRecords} />
      </div>
    </div>
  )
}
