import type { InvocationRecord, OverviewStats, ToolStat } from './types'

const API_BASE = process.env.NEXT_PUBLIC_API_BASE ?? 'http://localhost:7070'

export const WS_BASE = API_BASE.replace(/^http/, 'ws')

async function fetchJson<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(`${API_BASE}${path}`, init)
  if (!res.ok) {
    const err = new Error(`HTTP ${res.status}`) as Error & { status: number }
    err.status = res.status
    throw err
  }
  return res.json() as Promise<T>
}

export async function getInvocations(params: {
  limit?: number
  offset?: number
  tool?: string
  status?: string
}): Promise<InvocationRecord[]> {
  const qs = new URLSearchParams()
  if (params.limit != null) qs.set('limit', String(params.limit))
  if (params.offset != null) qs.set('offset', String(params.offset))
  if (params.tool) qs.set('tool', params.tool)
  if (params.status) qs.set('status', params.status)
  const query = qs.toString() ? `?${qs}` : ''
  return fetchJson<InvocationRecord[]>(`/api/invocations${query}`)
}

export async function getInvocation(id: string): Promise<InvocationRecord> {
  return fetchJson<InvocationRecord>(`/api/invocations/${id}`)
}

export async function getOverviewStats(): Promise<OverviewStats> {
  return fetchJson<OverviewStats>('/api/stats/overview')
}

export async function getToolStats(): Promise<ToolStat[]> {
  return fetchJson<ToolStat[]>('/api/stats/tools')
}

export async function getPolicies(): Promise<string> {
  const res = await fetch(`${API_BASE}/api/policies`)
  if (res.status === 404) return ''
  if (!res.ok) throw new Error(`HTTP ${res.status}`)
  return res.text()
}

export async function putPolicies(body: string): Promise<{ ok: boolean }> {
  const res = await fetch(`${API_BASE}/api/policies`, {
    method: 'PUT',
    headers: { 'Content-Type': 'text/plain' },
    body,
  })
  return { ok: res.ok }
}
