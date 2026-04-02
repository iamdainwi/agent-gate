interface KpiCardProps {
  title: string
  value: string | number
  subtitle?: string
}

export default function KpiCard({ title, value, subtitle }: KpiCardProps) {
  return (
    <div className="bg-gray-900 rounded-xl p-5 border border-gray-800">
      <p className="text-sm text-gray-400 font-medium">{title}</p>
      <p className="text-3xl font-bold text-white mt-1">{value}</p>
      {subtitle && <p className="text-xs text-gray-500 mt-1">{subtitle}</p>}
    </div>
  )
}
