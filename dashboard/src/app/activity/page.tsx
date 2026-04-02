'use client'

import { useEffect, useState } from 'react'
import { getInvocations } from '@/lib/api'
import type { InvocationRecord } from '@/lib/types'
import InvocationsTable from '@/components/InvocationsTable'

const LIMIT_OPTIONS = [50, 100, 200]
const STATUS_OPTIONS = ['', 'allowed', 'denied', 'error', 'rate_limited']

export default function ActivityPage() {
  const [tool, setTool] = useState('')
  const [status, setStatus] = useState('')
  const [limit, setLimit] = useState(50)
  const [offset, setOffset] = useState(0)
  const [records, setRecords] = useState<InvocationRecord[]>([])
  const [loading, setLoading] = useState(false)

  useEffect(() => {
    setLoading(true)
    getInvocations({ limit, offset, tool: tool || undefined, status: status || undefined })
      .then(setRecords)
      .catch(console.error)
      .finally(() => setLoading(false))
  }, [limit, offset, tool, status])

  function handleFilterChange() {
    setOffset(0)
  }

  return (
    <div className="space-y-5">
      <h1 className="text-2xl font-bold text-white">Activity</h1>

      <div className="flex flex-wrap gap-3 items-end">
        <div>
          <label className="block text-xs text-gray-400 mb-1">Tool</label>
          <input
            type="text"
            value={tool}
            onChange={(e) => { setTool(e.target.value); handleFilterChange() }}
            placeholder="Filter by tool…"
            className="bg-gray-800 border border-gray-700 rounded-md px-3 py-2 text-sm text-gray-100 placeholder-gray-500 focus:outline-none focus:ring-1 focus:ring-indigo-500"
          />
        </div>
        <div>
          <label className="block text-xs text-gray-400 mb-1">Status</label>
          <select
            value={status}
            onChange={(e) => { setStatus(e.target.value); handleFilterChange() }}
            className="bg-gray-800 border border-gray-700 rounded-md px-3 py-2 text-sm text-gray-100 focus:outline-none focus:ring-1 focus:ring-indigo-500"
          >
            {STATUS_OPTIONS.map((s) => (
              <option key={s} value={s}>{s || 'All statuses'}</option>
            ))}
          </select>
        </div>
        <div>
          <label className="block text-xs text-gray-400 mb-1">Limit</label>
          <select
            value={limit}
            onChange={(e) => { setLimit(Number(e.target.value)); handleFilterChange() }}
            className="bg-gray-800 border border-gray-700 rounded-md px-3 py-2 text-sm text-gray-100 focus:outline-none focus:ring-1 focus:ring-indigo-500"
          >
            {LIMIT_OPTIONS.map((l) => (
              <option key={l} value={l}>{l}</option>
            ))}
          </select>
        </div>
      </div>

      {loading ? (
        <p className="text-gray-500 text-sm">Loading…</p>
      ) : (
        <InvocationsTable records={records} />
      )}

      <div className="flex items-center gap-3">
        <button
          onClick={() => setOffset(Math.max(0, offset - limit))}
          disabled={offset === 0}
          className="px-4 py-2 rounded-md bg-gray-800 text-sm text-gray-300 hover:bg-gray-700 disabled:opacity-40 disabled:cursor-not-allowed"
        >
          ← Prev
        </button>
        <span className="text-xs text-gray-500">Offset: {offset}</span>
        <button
          onClick={() => setOffset(offset + limit)}
          disabled={records.length < limit}
          className="px-4 py-2 rounded-md bg-gray-800 text-sm text-gray-300 hover:bg-gray-700 disabled:opacity-40 disabled:cursor-not-allowed"
        >
          Next →
        </button>
      </div>
    </div>
  )
}
