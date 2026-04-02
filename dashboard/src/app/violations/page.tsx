'use client'

import { useEffect, useState } from 'react'
import { getInvocations } from '@/lib/api'
import type { InvocationRecord } from '@/lib/types'
import InvocationsTable from '@/components/InvocationsTable'

type PolicyGroup = { policy: string; records: InvocationRecord[] }

export default function ViolationsPage() {
  const [groups, setGroups] = useState<PolicyGroup[]>([])
  const [loading, setLoading] = useState(true)

  useEffect(() => {
    Promise.all([
      getInvocations({ status: 'denied', limit: 200 }),
      getInvocations({ status: 'rate_limited', limit: 200 }),
    ])
      .then(([denied, rateLimited]) => {
        const all = [...denied, ...rateLimited].sort(
          (a, b) => new Date(b.timestamp).getTime() - new Date(a.timestamp).getTime()
        )

        const map = new Map<string, InvocationRecord[]>()
        for (const r of all) {
          const key = r.policy_hit ?? '(no policy label)'
          if (!map.has(key)) map.set(key, [])
          map.get(key)!.push(r)
        }

        setGroups(
          Array.from(map.entries())
            .map(([policy, records]) => ({ policy, records }))
            .sort((a, b) => b.records.length - a.records.length)
        )
      })
      .catch(console.error)
      .finally(() => setLoading(false))
  }, [])

  return (
    <div className="space-y-6">
      <h1 className="text-2xl font-bold text-white">Violations</h1>

      {loading && <p className="text-gray-500 text-sm">Loading…</p>}

      {!loading && groups.length === 0 && (
        <p className="text-gray-500 text-sm">No violations found.</p>
      )}

      {groups.map(({ policy, records }) => (
        <section key={policy} className="space-y-3">
          <div className="flex items-center gap-3">
            <h2 className="text-base font-semibold text-white font-mono">{policy}</h2>
            <span className="bg-red-900 text-red-300 text-xs px-2 py-0.5 rounded-full font-medium">
              {records.length} {records.length === 1 ? 'hit' : 'hits'}
            </span>
          </div>
          <InvocationsTable records={records} />
        </section>
      ))}
    </div>
  )
}
