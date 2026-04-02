'use client'

import { useEffect, useState } from 'react'
import { getPolicies, putPolicies } from '@/lib/api'

type Banner = { type: 'success' | 'error'; message: string } | null

export default function SettingsPage() {
  const [content, setContent] = useState('')
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [notFound, setNotFound] = useState(false)
  const [banner, setBanner] = useState<Banner>(null)

  useEffect(() => {
    getPolicies()
      .then((text) => {
        if (text === '') setNotFound(true)
        setContent(text)
      })
      .catch(() => setNotFound(true))
      .finally(() => setLoading(false))
  }, [])

  async function handleSave() {
    setSaving(true)
    setBanner(null)
    try {
      const result = await putPolicies(content)
      if (result.ok) {
        setBanner({ type: 'success', message: 'Policy saved successfully.' })
        setNotFound(false)
      } else {
        setBanner({ type: 'error', message: 'Server rejected the policy update.' })
      }
    } catch {
      setBanner({ type: 'error', message: 'Failed to reach the backend.' })
    } finally {
      setSaving(false)
    }
  }

  return (
    <div className="space-y-5 flex flex-col h-full">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold text-white">Settings — Policy Editor</h1>
        <button
          onClick={handleSave}
          disabled={saving || loading}
          className="px-4 py-2 rounded-md bg-indigo-600 hover:bg-indigo-500 text-white text-sm font-medium disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
        >
          {saving ? 'Saving…' : 'Save Policy'}
        </button>
      </div>

      {banner && (
        <div
          className={`px-4 py-3 rounded-md text-sm font-medium ${
            banner.type === 'success'
              ? 'bg-green-900 text-green-300 border border-green-700'
              : 'bg-red-900 text-red-300 border border-red-700'
          }`}
        >
          {banner.message}
        </div>
      )}

      {notFound && !loading && (
        <p className="text-gray-500 text-sm">
          No policy file is currently configured. You can write a new policy below and save it.
        </p>
      )}

      {loading ? (
        <p className="text-gray-500 text-sm">Loading…</p>
      ) : (
        <textarea
          value={content}
          onChange={(e) => setContent(e.target.value)}
          spellCheck={false}
          className="flex-1 min-h-[60vh] w-full bg-gray-900 border border-gray-700 rounded-lg p-4 font-mono text-sm text-gray-100 placeholder-gray-600 focus:outline-none focus:ring-1 focus:ring-indigo-500 resize-y"
          placeholder="# Paste or write your TOML policy here…"
        />
      )}
    </div>
  )
}
