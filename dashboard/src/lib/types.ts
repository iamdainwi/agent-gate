export type InvocationStatus = 'allowed' | 'denied' | 'error' | 'rate_limited'

export interface InvocationRecord {
  id: string
  timestamp: string
  agent_id: string | null
  session_id: string | null
  server_name: string
  tool_name: string
  arguments: unknown | null
  result: unknown | null
  latency_ms: number | null
  status: InvocationStatus
  policy_hit: string | null
}

export interface OverviewStats {
  total_calls: number
  total_denials: number
  avg_latency_ms: number | null
  calls_per_minute_now: number
  sparkline: { bucket: string; count: number }[]
}

export interface ToolStat {
  tool_name: string
  total_calls: number
  error_count: number
  denial_count: number
  avg_latency_ms: number | null
  last_seen: string
}
