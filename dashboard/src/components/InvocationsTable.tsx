'use client'

import type { InvocationRecord } from '@/lib/types'
import StatusBadge from './StatusBadge'

interface Props {
  records: InvocationRecord[]
}

export default function InvocationsTable({ records }: Props) {
  if (records.length === 0) {
    return <p className="text-gray-500 text-sm py-4">No records found.</p>
  }

  return (
    <div className="overflow-x-auto rounded-lg border border-gray-800">
      <table className="w-full text-sm text-left">
        <thead className="bg-gray-900 text-gray-400 text-xs uppercase tracking-wider">
          <tr>
            <th className="px-4 py-3">Timestamp</th>
            <th className="px-4 py-3">Server</th>
            <th className="px-4 py-3">Tool</th>
            <th className="px-4 py-3">Status</th>
            <th className="px-4 py-3">Latency (ms)</th>
            <th className="px-4 py-3">Policy Hit</th>
          </tr>
        </thead>
        <tbody className="divide-y divide-gray-800">
          {records.map((r) => (
            <tr key={r.id} className="bg-gray-950 hover:bg-gray-900 transition-colors cursor-default">
              <td className="px-4 py-3 text-gray-300 whitespace-nowrap">
                {new Date(r.timestamp).toLocaleString()}
              </td>
              <td className="px-4 py-3 text-gray-300">{r.server_name}</td>
              <td className="px-4 py-3 font-mono text-indigo-300">{r.tool_name}</td>
              <td className="px-4 py-3">
                <StatusBadge status={r.status} />
              </td>
              <td className="px-4 py-3 text-gray-300">
                {r.latency_ms != null ? r.latency_ms.toFixed(1) : '—'}
              </td>
              <td className="px-4 py-3 text-gray-400 font-mono text-xs">
                {r.policy_hit ?? '—'}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}
