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

export type IllustrationBinding = {
  entity_type: string
  entity_key: string
  media_role: string
  asset_key: string
  alt: string
  width: number
  height: number
}

export type DirectIllustrationUploadResult = {
  asset_key: string
  byte_size: number
  height: number
  mime_type: string
  width: number
}

export type ContentValidation = {
  package_key: string
  package_revision: number
  content_hash: string
  valid: boolean
  errors: string[]
  item_count: number
  wuhun_count: number
  skill_count: number
  effect_count: number
  soul_beast_count: number
  soul_ring_count: number
}

export type ContentDraftDiffMember = {
  member_kind: string
  member_key: string
}

export type ContentDraftDiffPreview = {
  draft: ContentDraft
  active_revision: ContentRevision
  active_member_count: number
  added_members: ContentDraftDiffMember[]
  projected_member_count: number
}

export type ContentStageResult = {
  draft: ContentDraft
  replayed: boolean
}

export type ContentPublishResult = {
  revision: ContentRevision
  active_revision_id: number
  member_count: number
  replayed: boolean
}

export type ContentRollbackResult = {
  revision: ContentRevision
  active_revision_id: number
  activation_id: number
}

export type PlayerStageCandidate = {
  source_player_id: number
  subject_id: string
  name: string
  gender: string
  level: number
  exp: number
  hp: number
  max_hp: number
  soul_power: number
  max_soul_power: number
  strength: number
  agility: number
  spirit: number
  endurance: number
  perception: number
  luck: number
  life_count: number
}

export type PlayerStageCandidates = {
  protocol: string
  account_id: string
  namespace: string
  staged_at: number
  total_players: number
  ready_players: number
  rejected_players: number
  entries: PlayerStageCandidate[]
  next_after_source_player_id: number | null
}

export type PlayerStageConfirmation = {
  player_id: number
  source_player_id: number
  name: string
  level: number
  map_name: string
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

export function listIllustrations(): Promise<{ entries: IllustrationBinding[] }> {
  return request<{ entries: IllustrationBinding[] }>('/api/v1/illustrations')
}

// 图片二进制只发给同源管理端，资源键和 CSRF token 通过受控请求头传递。
export function uploadDirectIllustration(
  assetKey: string,
  file: File,
  csrfToken: string,
): Promise<DirectIllustrationUploadResult> {
  return request<DirectIllustrationUploadResult>('/api/v1/illustrations/upload', {
    body: file,
    headers: {
      'content-type': file.type || 'application/octet-stream',
      'x-csrf-token': csrfToken,
      'x-illustration-asset-key': assetKey,
    },
    method: 'POST',
  })
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

// 写操作只复用后端 Store 事务，并把页面内存中的 CSRF token 交给同源请求。
export function stageContentDraft(
  packageFile: string,
  csrfToken: string,
): Promise<ContentStageResult> {
  return request<ContentStageResult>('/api/v1/content/drafts/stage', {
    body: JSON.stringify({ package_file: packageFile }),
    headers: { 'x-csrf-token': csrfToken },
    method: 'POST',
  })
}

function draftActionPath(packageKey: string, packageRevision: number, action: 'validate' | 'publish') {
  return `/api/v1/content/drafts/${encodeURIComponent(packageKey)}/${packageRevision}/${action}`
}

export function getContentDraftDiff(
  packageKey: string,
  packageRevision: number,
): Promise<ContentDraftDiffPreview> {
  return request<ContentDraftDiffPreview>(
    `/api/v1/content/drafts/${encodeURIComponent(packageKey)}/${packageRevision}/diff`,
  )
}

export function validateContentDraft(
  packageKey: string,
  packageRevision: number,
  csrfToken: string,
): Promise<ContentValidation> {
  return request<ContentValidation>(draftActionPath(packageKey, packageRevision, 'validate'), {
    headers: { 'x-csrf-token': csrfToken },
    method: 'POST',
  })
}

export function publishContentDraft(
  packageKey: string,
  packageRevision: number,
  csrfToken: string,
): Promise<ContentPublishResult> {
  return request<ContentPublishResult>(draftActionPath(packageKey, packageRevision, 'publish'), {
    headers: { 'x-csrf-token': csrfToken },
    method: 'POST',
  })
}

export function rollbackContentRevision(
  revisionId: number,
  csrfToken: string,
): Promise<ContentRollbackResult> {
  return request<ContentRollbackResult>(`/api/v1/content/revisions/${revisionId}/rollback`, {
    headers: { 'x-csrf-token': csrfToken },
    method: 'POST',
  })
}

export function listPlayerStageCandidates(
  stageFile: string,
  afterSourcePlayerId?: number,
): Promise<PlayerStageCandidates> {
  const query = new URLSearchParams({ limit: '20', stage_file: stageFile })
  if (afterSourcePlayerId !== undefined) {
    query.set('after_source_player_id', String(afterSourcePlayerId))
  }
  return request<PlayerStageCandidates>(`/api/v1/player-staging/candidates?${query.toString()}`)
}

export function confirmPlayerStage(
  stageFile: string,
  sourcePlayerId: number,
  csrfToken: string,
): Promise<PlayerStageConfirmation> {
  return request<PlayerStageConfirmation>('/api/v1/player-staging/confirm', {
    body: JSON.stringify({ stage_file: stageFile, source_player_id: sourcePlayerId }),
    headers: { 'x-csrf-token': csrfToken },
    method: 'POST',
  })
}
