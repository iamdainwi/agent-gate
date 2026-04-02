'use client'

import Link from 'next/link'
import { usePathname } from 'next/navigation'

const NAV_ITEMS = [
  { href: '/', label: 'Overview' },
  { href: '/activity', label: 'Activity' },
  { href: '/violations', label: 'Violations' },
  { href: '/analytics', label: 'Analytics' },
  { href: '/settings', label: 'Settings' },
]

export default function Sidebar() {
  const pathname = usePathname()

  return (
    <aside className="w-[200px] shrink-0 bg-gray-900 flex flex-col h-full border-r border-gray-800">
      <div className="px-5 py-5 border-b border-gray-800">
        <span className="text-lg font-bold tracking-tight text-white">AgentGate</span>
      </div>
      <nav className="flex-1 py-4">
        {NAV_ITEMS.map(({ href, label }) => {
          const active = pathname === href || (href !== '/' && pathname.startsWith(href))
          return (
            <Link
              key={href}
              href={href}
              className={`flex items-center px-5 py-2.5 text-sm font-medium rounded-md mx-2 mb-1 transition-colors ${
                active
                  ? 'bg-indigo-600 text-white'
                  : 'text-gray-400 hover:text-white hover:bg-gray-800'
              }`}
            >
              {label}
            </Link>
          )
        })}
      </nav>
    </aside>
  )
}
