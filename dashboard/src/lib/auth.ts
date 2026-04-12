const STORAGE_KEY = 'agentgate_token'

export function getToken(): string | null {
  if (typeof window === 'undefined') return null
  return sessionStorage.getItem(STORAGE_KEY)
}

export function setToken(token: string): void {
  sessionStorage.setItem(STORAGE_KEY, token)
}

export function clearToken(): void {
  sessionStorage.removeItem(STORAGE_KEY)
}

/** Thrown by api.ts when the server responds with 401. */
export class AuthError extends Error {
  constructor() {
    super('Unauthorized')
    this.name = 'AuthError'
  }
}
