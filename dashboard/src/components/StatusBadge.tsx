import type { InvocationStatus } from '@/lib/types'

const STATUS_STYLES: Record<InvocationStatus, string> = {
  allowed: 'bg-green-900 text-green-300',
  denied: 'bg-red-900 text-red-300',
  error: 'bg-yellow-900 text-yellow-300',
  rate_limited: 'bg-orange-900 text-orange-300',
}

export default function StatusBadge({ status }: { status: InvocationStatus }) {
  return (
    <span className={`inline-flex items-center px-2 py-0.5 rounded-full text-xs font-medium ${STATUS_STYLES[status]}`}>
      {status.replace('_', ' ')}
    </span>
  )
}
