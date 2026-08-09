export type Session = {
  role: string
  csrf_token: string
  expires_in_seconds: number
}

export type ContentRevision = {
  id: number
  package_key: string
  package_revision: number
  parent_revision_id: number | null
  source_format: string
  content_hash: string
  author: string
  minimum_runtime: string
  published_at: number
}

export type ContentDraft = {
  id: number
  package_key: string
  package_revision: number
  source_format: string
  content_hash: string
  status: string
  validation_errors: string[]
  published_revision_id: number | null
  created_at: number
  updated_at: number
}

export type ContentRevisionSummary = {
  revision: ContentRevision
  member_count: number
}

export type ContentActivation = {
  id: number
  revision_id: number
  reason: string
  created_at: number
}

export type ContentOperation = {
  id: number
  actor_role: string
  action: string
  package_key: string
  package_revision: number
  content_hash: string
  outcome: string
  revision_id: number | null
  created_at: number
}

export type ContentRollbackOperation = {
  id: number
  actor_role: string
  revision_id: number
  activation_id: number
  created_at: number
}

export type ContentStageOperation = {
  id: number
  actor_role: string
  package_key: string
  package_revision: number
  content_hash: string
  source_format: string
  outcome: string
  created_at: number
}

export type CursorPage<T> = {
  entries: T[]
  next_after_id: number | null
}

type ErrorBody = {
  error?: string
}

export class ManagementApiError extends Error {
  readonly status: number
  readonly code: string

  constructor(status: number, code: string) {
    super(code)
    this.status = status
    this.code = code
  }
}

async function request<T>(path: string, init: RequestInit = {}): Promise<T> {
  // 管理端只允许同源、无缓存的请求，cookie 始终交由浏览器的 HttpOnly 会话管理。
  const headers = new Headers(init.headers)
  headers.set('accept', 'application/json')
  if (init.body && !headers.has('content-type')) {
    headers.set('content-type', 'application/json')
  }

  const response = await fetch(path, {
    ...init,
    cache: 'no-store',
    credentials: 'same-origin',
    headers,
  })
  const text = response.status === 204 ? '' : await response.text()
  const payload = text ? tryParseJson(text) : undefined

  if (!response.ok) {
    const code = isErrorBody(payload) ? payload.error ?? 'request_failed' : 'request_failed'
    throw new ManagementApiError(response.status, code)
  }

  return payload as T
}

function tryParseJson(text: string): unknown {
  try {
    return JSON.parse(text)
  } catch {
    return undefined
  }
}

function isErrorBody(value: unknown): value is ErrorBody {
  return typeof value === 'object' && value !== null && 'error' in value
}

function cursorPath(path: string, afterId?: number): string {
  const query = new URLSearchParams({ limit: '20' })
  if (afterId !== undefined) {
    query.set('after_id', String(afterId))
  }
  return `${path}?${query.toString()}`
}

export async function getSession(): Promise<Session | null> {
  try {
    return await request<Session>('/api/v1/session')
  } catch (error) {
    if (error instanceof ManagementApiError && error.status === 401) {
      return null
    }
    throw error
  }
}

export function login(secret: string): Promise<Session> {
  return request<Session>('/api/v1/session', {
    body: JSON.stringify({ secret }),
    method: 'POST',
  })
}

export function logout(csrfToken: string): Promise<void> {
  return request<void>('/api/v1/session', {
    headers: { 'x-csrf-token': csrfToken },
    method: 'DELETE',
  })
}

export function getActiveRevision(): Promise<{ revision: ContentRevision }> {
  return request<{ revision: ContentRevision }>('/api/v1/content/active')
}

export function listDrafts(afterId?: number): Promise<CursorPage<ContentDraft>> {
  return request<CursorPage<ContentDraft>>(cursorPath('/api/v1/content/drafts', afterId))
}

export function listRevisions(afterId?: number): Promise<CursorPage<ContentRevisionSummary>> {
  return request<CursorPage<ContentRevisionSummary>>(cursorPath('/api/v1/content/revisions', afterId))
}

export function listActivations(afterId?: number): Promise<CursorPage<ContentActivation>> {
  return request<CursorPage<ContentActivation>>(cursorPath('/api/v1/content/activations', afterId))
}

export function listOperations(afterId?: number): Promise<CursorPage<ContentOperation>> {
  return request<CursorPage<ContentOperation>>(cursorPath('/api/v1/content/operations', afterId))
}

export function listRollbackOperations(
  afterId?: number,
): Promise<CursorPage<ContentRollbackOperation>> {
  return request<CursorPage<ContentRollbackOperation>>(
    cursorPath('/api/v1/content/rollback-operations', afterId),
  )
}

export function listStageOperations(afterId?: number): Promise<CursorPage<ContentStageOperation>> {
  return request<CursorPage<ContentStageOperation>>(
    cursorPath('/api/v1/content/stage-operations', afterId),
  )
}
