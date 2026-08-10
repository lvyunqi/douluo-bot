import { type ReactNode, useCallback, useEffect, useMemo, useState } from 'react'
import {
  Activity,
  AlertTriangle,
  BookOpenCheck,
  ChevronDown,
  ClipboardList,
  Database,
  Eye,
  FileUp,
  History,
  Image,
  LogOut,
  RefreshCw,
  ScrollText,
  ShieldCheck,
} from 'lucide-react'

import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { ContentWritePanel, type WriteFeedback } from '@/components/management/content-write-panel'
import { ContentDraftDiffPreviewPanel } from '@/components/management/content-draft-diff-preview'
import { Separator } from '@/components/ui/separator'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import {
  getActiveRevision,
  getContentDraftDiff,
  listIllustrations,
  listActivations,
  listDrafts,
  listOperations,
  listRevisions,
  listRollbackOperations,
  listStageOperations,
  ManagementApiError,
  publishContentDraft,
  rollbackContentRevision,
  stageContentDraft,
  validateContentDraft,
  logout,
  type ContentActivation,
  type ContentDraft,
  type ContentDraftDiffPreview,
  type ContentOperation,
  type ContentRevision,
  type ContentRevisionSummary,
  type ContentRollbackOperation,
  type ContentStageOperation,
  type CursorPage,
  type IllustrationBinding,
  type Session,
} from '@/lib/api'
import { formatNumber, formatTimestamp, packageLabel, shortHash, statusLabel, statusVariant } from '@/lib/format'

type TabValue = 'overview' | 'illustrations' | 'operations' | 'drafts' | 'revisions' | 'activations' | 'audits'
type PageValue =
  | ContentDraft
  | ContentRevisionSummary
  | ContentActivation
  | ContentOperation
  | ContentRollbackOperation
  | ContentStageOperation

type PageKey =
  | 'drafts'
  | 'revisions'
  | 'activations'
  | 'operations'
  | 'rollbackOperations'
  | 'stageOperations'

type DashboardSnapshot = {
  active: ContentRevision
  illustrations: IllustrationBinding[]
  drafts: CursorPage<ContentDraft>
  revisions: CursorPage<ContentRevisionSummary>
  activations: CursorPage<ContentActivation>
  operations: CursorPage<ContentOperation>
  rollbackOperations: CursorPage<ContentRollbackOperation>
  stageOperations: CursorPage<ContentStageOperation>
}

type ManagementDashboardProps = {
  onSessionExpired: () => void
  onSignedOut: () => void
  session: Session
}

type TableColumn<T> = {
  className?: string
  header: string
  render: (item: T) => ReactNode
}

const tabs: Array<{ icon: typeof Activity; label: string; value: TabValue }> = [
  { icon: Activity, label: '概览', value: 'overview' },
  { icon: Image, label: '插图', value: 'illustrations' },
  { icon: FileUp, label: '写入', value: 'operations' },
  { icon: ClipboardList, label: '草稿', value: 'drafts' },
  { icon: BookOpenCheck, label: '版本', value: 'revisions' },
  { icon: History, label: '激活', value: 'activations' },
  { icon: ScrollText, label: '审计', value: 'audits' },
]

const pageLoaders: Record<PageKey, (afterId: number) => Promise<CursorPage<PageValue>>> = {
  activations: listActivations,
  drafts: listDrafts,
  operations: listOperations,
  revisions: listRevisions,
  rollbackOperations: listRollbackOperations,
  stageOperations: listStageOperations,
}

function requestMessage(error: unknown): string {
  if (error instanceof ManagementApiError && error.status === 401) {
    return '管理会话已失效。'
  }
  return '无法读取内容管理数据。'
}

function writeErrorMessage(error: unknown): string {
  if (error instanceof ManagementApiError) {
    if (error.status === 401) {
      return '管理会话已失效。'
    }
    if (error.status === 403 && error.code === 'csrf_required') {
      return '会话校验已失效，请重新登录。'
    }
    if (error.code === 'published_draft_conflict') {
      return '该草稿身份已发布但内容哈希不同，请提高内容 revision 后重新暂存。'
    }
    if (error.code === 'draft_not_validated') {
      return '草稿状态已变化，请重新校验后再发布。'
    }
    if (error.code === 'draft_rejected') {
      return '校验已拒绝，请修正已部署的内容文件后重新暂存。'
    }
    if (error.code === 'invalid_package') {
      return '内容包格式或结构校验失败。'
    }
    if (error.code === 'not_found') {
      return '目标已不存在，请刷新后重新选择。'
    }
  }
  return '操作未完成，请稍后重试。'
}

function DataTable<T>({
  columns,
  emptyLabel,
  entries,
  rowKey,
}: {
  columns: Array<TableColumn<T>>
  emptyLabel: string
  entries: T[]
  rowKey: (item: T) => string | number
}) {
  return (
    <div className="overflow-x-auto">
      <Table>
        <TableHeader>
          <TableRow>
            {columns.map((column) => (
              <TableHead className={column.className} key={column.header}>
                {column.header}
              </TableHead>
            ))}
          </TableRow>
        </TableHeader>
        <TableBody>
          {entries.length ? (
            entries.map((entry) => (
              <TableRow key={rowKey(entry)}>
                {columns.map((column) => (
                  <TableCell className={column.className} key={column.header}>
                    {column.render(entry)}
                  </TableCell>
                ))}
              </TableRow>
            ))
          ) : (
            <TableRow>
              <TableCell className="h-28 text-center text-muted-foreground" colSpan={columns.length}>
                {emptyLabel}
              </TableCell>
            </TableRow>
          )}
        </TableBody>
      </Table>
    </div>
  )
}

function PagePanel<T>({
  columns,
  description,
  disabled,
  emptyLabel,
  entries,
  isLoadingMore,
  nextAfterId,
  onLoadMore,
  rowKey,
  title,
}: {
  columns: Array<TableColumn<T>>
  description: string
  disabled: boolean
  emptyLabel: string
  entries: T[]
  isLoadingMore: boolean
  nextAfterId: number | null
  onLoadMore: () => void
  rowKey: (item: T) => string | number
  title: string
}) {
  return (
    <section className="space-y-3">
      <div className="flex flex-wrap items-end justify-between gap-3">
        <div>
          <h2 className="text-base font-semibold">{title}</h2>
          <p className="mt-1 text-sm text-muted-foreground">{description}</p>
        </div>
        <Badge variant="outline">{formatNumber(entries.length)} 条</Badge>
      </div>
      <Card className="rounded-lg py-0 shadow-sm">
        <CardContent className="p-0">
          <DataTable columns={columns} emptyLabel={emptyLabel} entries={entries} rowKey={rowKey} />
        </CardContent>
      </Card>
      {nextAfterId !== null ? (
        <div className="flex justify-center">
          <Button disabled={disabled || isLoadingMore} onClick={onLoadMore} variant="outline">
            <ChevronDown data-icon="inline-start" aria-hidden="true" />
            {isLoadingMore ? '读取中' : '加载更多'}
          </Button>
        </div>
      ) : null}
    </section>
  )
}

function DashboardSkeleton() {
  return (
    <main className="grid min-h-svh bg-background lg:grid-cols-[15rem_minmax(0,1fr)]">
      <aside className="hidden bg-sidebar lg:block" />
      <section className="space-y-6 p-5 sm:p-8">
        <Skeleton className="h-10 w-64" />
        <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
          {Array.from({ length: 4 }, (_, index) => (
            <Skeleton className="h-28" key={index} />
          ))}
        </div>
        <Skeleton className="h-72" />
      </section>
    </main>
  )
}

export function ManagementDashboard({
  onSessionExpired,
  onSignedOut,
  session,
}: ManagementDashboardProps) {
  const [activeTab, setActiveTab] = useState<TabValue>('overview')
  const [snapshot, setSnapshot] = useState<DashboardSnapshot | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)
  const [loadingMore, setLoadingMore] = useState<PageKey | null>(null)
  const [pendingAction, setPendingAction] = useState<string | null>(null)
  const [signingOut, setSigningOut] = useState(false)
  const [writeFeedback, setWriteFeedback] = useState<WriteFeedback | null>(null)
  const [draftDiffPreview, setDraftDiffPreview] = useState<ContentDraftDiffPreview | null>(null)
  const [draftDiffDraft, setDraftDiffDraft] = useState<ContentDraft | null>(null)
  const [draftDiffLoadingId, setDraftDiffLoadingId] = useState<number | null>(null)
  const [draftDiffError, setDraftDiffError] = useState<string | null>(null)

  const loadSnapshot = useCallback(async (): Promise<boolean> => {
    setLoading(true)
    setError(null)
    try {
      const [active, illustrations, drafts, revisions, activations, operations, rollbackOperations, stageOperations] =
        await Promise.all([
          getActiveRevision(),
          listIllustrations(),
          listDrafts(),
          listRevisions(),
          listActivations(),
          listOperations(),
          listRollbackOperations(),
          listStageOperations(),
        ])
      setSnapshot({
        active: active.revision,
        activations,
        drafts,
        illustrations: illustrations.entries,
        operations,
        revisions,
        rollbackOperations,
        stageOperations,
      })
      return true
    } catch (requestError) {
      if (requestError instanceof ManagementApiError && requestError.status === 401) {
        onSessionExpired()
        return false
      }
      setError(requestMessage(requestError))
      return false
    } finally {
      setLoading(false)
    }
  }, [onSessionExpired])

  useEffect(() => {
    void loadSnapshot()
  }, [loadSnapshot])

  async function loadMore(key: PageKey) {
    if (!snapshot || loadingMore || pendingAction || loading) {
      return
    }
    const page = snapshot[key] as CursorPage<PageValue>
    if (page.next_after_id === null) {
      return
    }

    setLoadingMore(key)
    try {
      const next = await pageLoaders[key](page.next_after_id)
      setSnapshot((current) => {
        if (!current) {
          return current
        }
        const currentPage = current[key] as CursorPage<PageValue>
        return {
          ...current,
          [key]: {
            entries: [...currentPage.entries, ...next.entries],
            next_after_id: next.next_after_id,
          },
        } as DashboardSnapshot
      })
    } catch (requestError) {
      if (requestError instanceof ManagementApiError && requestError.status === 401) {
        onSessionExpired()
        return
      }
      setError(requestMessage(requestError))
    } finally {
      setLoadingMore(null)
    }
  }

  async function runContentWrite<T>(
    actionKey: string,
    operation: () => Promise<T>,
    successDescription: (result: T) => string,
  ): Promise<boolean> {
    if (pendingAction || loadingMore || loading || draftDiffLoadingId !== null) {
      return false
    }
    setPendingAction(actionKey)
    setWriteFeedback(null)
    setDraftDiffPreview(null)
    setDraftDiffDraft(null)
    setDraftDiffError(null)
    setError(null)
    try {
      const result = await operation()
      const refreshed = await loadSnapshot()
      setWriteFeedback({
        description: refreshed
          ? successDescription(result)
          : 'Store 已接受操作，但刷新管理快照失败，请手动刷新确认当前状态。',
        kind: refreshed ? 'success' : 'error',
        title: refreshed ? '操作完成' : '写入已提交',
      })
      return true
    } catch (requestError) {
      if (requestError instanceof ManagementApiError && requestError.status === 401) {
        onSessionExpired()
        return false
      }
      if (
        requestError instanceof ManagementApiError &&
        (requestError.status === 404 || requestError.status === 409)
      ) {
        await loadSnapshot()
      }
      setWriteFeedback({
        description: writeErrorMessage(requestError),
        kind: 'error',
        title: '操作被拒绝',
      })
      return false
    } finally {
      setPendingAction(null)
    }
  }

  // 差异预览只读取已暂存草稿，不改变校验、发布或激活状态。
  const previewDraft = useCallback(
    async (draft: ContentDraft) => {
      if (loading || loadingMore || pendingAction || draftDiffLoadingId !== null) {
        return
      }
      setDraftDiffDraft(draft)
      setDraftDiffPreview(null)
      setDraftDiffError(null)
      setDraftDiffLoadingId(draft.id)
      try {
        const preview = await getContentDraftDiff(draft.package_key, draft.package_revision)
        setDraftDiffPreview(preview)
      } catch (requestError) {
        if (requestError instanceof ManagementApiError && requestError.status === 401) {
          onSessionExpired()
          return
        }
        if (requestError instanceof ManagementApiError && requestError.status === 404) {
          setDraftDiffError('草稿已不存在，请刷新内容列表后重试。')
          return
        }
        setDraftDiffError('无法读取草稿差异。')
      } finally {
        setDraftDiffLoadingId(null)
      }
    },
    [draftDiffLoadingId, loading, loadingMore, onSessionExpired, pendingAction],
  )

  function stageDraft(packageFile: string) {
    return runContentWrite(
      'stage',
      () => stageContentDraft(packageFile, session.csrf_token),
      (result) => (result.replayed ? '相同内容已幂等重放。' : `已暂存 ${packageLabel(result.draft.package_key, result.draft.package_revision)}。`),
    )
  }

  function validateDraft(draft: ContentDraft) {
    return runContentWrite(
      `validate:${draft.id}`,
      () => validateContentDraft(draft.package_key, draft.package_revision, session.csrf_token),
      (result) => (result.valid ? '草稿校验通过，可以发布。' : `校验完成，发现 ${result.errors.length} 项错误。`),
    )
  }

  function publishDraft(draft: ContentDraft) {
    return runContentWrite(
      `publish:${draft.id}`,
      () => publishContentDraft(draft.package_key, draft.package_revision, session.csrf_token),
      (result) =>
        result.replayed
          ? `已重新激活 revision #${result.active_revision_id}。`
          : `已发布并激活 revision #${result.active_revision_id}。`,
    )
  }

  function rollbackRevision(revision: ContentRevision) {
    return runContentWrite(
      `rollback:${revision.id}`,
      () => rollbackContentRevision(revision.id, session.csrf_token),
      (result) => `已追加回滚 activation #${result.activation_id}，当前 revision 为 #${result.active_revision_id}。`,
    )
  }

  async function signOut() {
    if (signingOut) {
      return
    }
    setSigningOut(true)
    try {
      await logout(session.csrf_token)
      onSignedOut()
    } catch (requestError) {
      if (requestError instanceof ManagementApiError && requestError.status === 401) {
        onSessionExpired()
        return
      }
      setError('无法结束管理会话。')
    } finally {
      setSigningOut(false)
    }
  }

  const dashboardDisabled =
    loading || loadingMore !== null || pendingAction !== null || draftDiffLoadingId !== null

  const draftColumns = useMemo<Array<TableColumn<ContentDraft>>>(
    () => [
      {
        header: '草稿',
        render: (draft) => (
          <div className="space-y-0.5">
            <p className="font-medium">{packageLabel(draft.package_key, draft.package_revision)}</p>
            <p className="font-mono text-xs text-muted-foreground">#{draft.id}</p>
          </div>
        ),
      },
      {
        header: '状态',
        render: (draft) => <Badge variant={statusVariant(draft.status)}>{statusLabel(draft.status)}</Badge>,
      },
      { header: '格式', render: (draft) => draft.source_format.toUpperCase() },
      {
        header: '校验',
        render: (draft) => (draft.validation_errors.length ? `${draft.validation_errors.length} 项` : '无错误'),
      },
      { header: '更新时间', render: (draft) => formatTimestamp(draft.updated_at) },
      {
        className: 'w-12 text-right',
        header: '预览',
        render: (draft) => (
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                aria-label={`预览 ${packageLabel(draft.package_key, draft.package_revision)} 的草稿差异`}
                disabled={dashboardDisabled || draftDiffLoadingId !== null}
                onClick={() => void previewDraft(draft)}
                size="icon"
                variant="ghost"
              >
                <Eye
                  aria-hidden="true"
                  className={draftDiffLoadingId === draft.id ? 'size-4 animate-pulse' : 'size-4'}
                />
              </Button>
            </TooltipTrigger>
            <TooltipContent>{draftDiffLoadingId === draft.id ? '正在加载差异' : '预览差异'}</TooltipContent>
          </Tooltip>
        ),
      },
    ],
    [dashboardDisabled, draftDiffLoadingId, previewDraft],
  )

  const illustrationColumns = useMemo<Array<TableColumn<IllustrationBinding>>>(
    () => [
      {
        header: '实体',
        render: (binding) => (
          <div className="space-y-0.5">
            <p className="font-medium">{binding.entity_key}</p>
            <p className="text-xs text-muted-foreground">{binding.entity_type}</p>
          </div>
        ),
      },
      { header: '用途', render: (binding) => <Badge variant="outline">{binding.media_role}</Badge> },
      {
        className: 'max-w-72 font-mono text-xs',
        header: '资源键',
        render: (binding) => <span className="block truncate">{binding.asset_key}</span>,
      },
      { className: 'max-w-48', header: '说明', render: (binding) => <span className="block truncate">{binding.alt}</span> },
      { header: '尺寸', render: (binding) => `${binding.width} × ${binding.height}` },
    ],
    [],
  )

  const revisionColumns = useMemo<Array<TableColumn<ContentRevisionSummary>>>(
    () => [
      {
        header: '内容 revision',
        render: ({ revision }) => (
          <div className="space-y-0.5">
            <p className="font-medium">{packageLabel(revision.package_key, revision.package_revision)}</p>
            <p className="font-mono text-xs text-muted-foreground">#{revision.id}</p>
          </div>
        ),
      },
      { header: '成员', render: ({ member_count }) => formatNumber(member_count) },
      { header: '来源', render: ({ revision }) => revision.source_format.toUpperCase() },
      { header: '发布者', render: ({ revision }) => revision.author || '未记录' },
      { header: '发布时间', render: ({ revision }) => formatTimestamp(revision.published_at) },
    ],
    [],
  )

  const activationColumns = useMemo<Array<TableColumn<ContentActivation>>>(
    () => [
      { header: '激活', render: (activation) => `#${activation.id}` },
      { header: 'revision', render: (activation) => `#${activation.revision_id}` },
      { header: '原因', render: (activation) => activation.reason },
      { header: '时间', render: (activation) => formatTimestamp(activation.created_at) },
    ],
    [],
  )

  const operationColumns = useMemo<Array<TableColumn<ContentOperation>>>(
    () => [
      { header: '操作', render: (operation) => <Badge variant="outline">{operation.action}</Badge> },
      { header: '草稿', render: (operation) => packageLabel(operation.package_key, operation.package_revision) },
      {
        header: '结果',
        render: (operation) => <Badge variant={statusVariant(operation.outcome)}>{statusLabel(operation.outcome)}</Badge>,
      },
      { header: 'revision', render: (operation) => (operation.revision_id ? `#${operation.revision_id}` : '—') },
      { header: '时间', render: (operation) => formatTimestamp(operation.created_at) },
    ],
    [],
  )

  const rollbackColumns = useMemo<Array<TableColumn<ContentRollbackOperation>>>(
    () => [
      { header: '回滚审计', render: (operation) => `#${operation.id}` },
      { header: '目标 revision', render: (operation) => `#${operation.revision_id}` },
      { header: 'activation', render: (operation) => `#${operation.activation_id}` },
      { header: '时间', render: (operation) => formatTimestamp(operation.created_at) },
    ],
    [],
  )

  const stageColumns = useMemo<Array<TableColumn<ContentStageOperation>>>(
    () => [
      { header: '暂存审计', render: (operation) => `#${operation.id}` },
      { header: '草稿', render: (operation) => packageLabel(operation.package_key, operation.package_revision) },
      { header: '格式', render: (operation) => operation.source_format.toUpperCase() },
      {
        header: '结果',
        render: (operation) => <Badge variant={statusVariant(operation.outcome)}>{statusLabel(operation.outcome)}</Badge>,
      },
      { header: '时间', render: (operation) => formatTimestamp(operation.created_at) },
    ],
    [],
  )

  if (loading && !snapshot) {
    return <DashboardSkeleton />
  }

  if (!snapshot) {
    return (
      <main className="grid min-h-svh place-items-center bg-background p-5">
        <Card className="w-full max-w-md rounded-lg shadow-sm">
          <CardHeader>
            <CardTitle>内容管理不可用</CardTitle>
            <CardDescription>{error ?? '无法读取当前内容 revision。'}</CardDescription>
          </CardHeader>
          <CardContent>
            <Button onClick={() => void loadSnapshot()}>
              <RefreshCw data-icon="inline-start" aria-hidden="true" />
              重试
            </Button>
          </CardContent>
        </Card>
      </main>
    )
  }

  return (
    <Tabs
      className="grid min-h-svh bg-background lg:grid-cols-[15rem_minmax(0,1fr)]"
      onValueChange={(value) => setActiveTab(value as TabValue)}
      value={activeTab}
    >
      <aside className="hidden min-h-svh flex-col border-r border-sidebar-border bg-sidebar text-sidebar-foreground lg:flex">
        <div className="flex h-16 items-center gap-3 border-b border-sidebar-border px-5">
          <span className="grid size-9 place-items-center rounded-lg bg-sidebar-primary text-sidebar-primary-foreground">
            <ShieldCheck className="size-5" aria-hidden="true" />
          </span>
          <span className="text-sm font-semibold">斗罗内容管理</span>
        </div>
        <nav className="flex-1 space-y-1 p-3" aria-label="内容管理视图">
          {tabs.map(({ icon: Icon, label, value }) => (
            <Button
              className="w-full justify-start text-sidebar-foreground hover:bg-sidebar-accent hover:text-sidebar-accent-foreground data-[active=true]:bg-sidebar-accent data-[active=true]:text-sidebar-accent-foreground"
              data-active={activeTab === value}
              key={value}
              onClick={() => setActiveTab(value)}
              variant="ghost"
            >
              <Icon data-icon="inline-start" aria-hidden="true" />
              {label}
            </Button>
          ))}
        </nav>
        <div className="border-t border-sidebar-border p-3 text-xs text-sidebar-foreground/65">
          <p>content_admin</p>
          <p className="mt-1">{session.expires_in_seconds} 秒</p>
        </div>
      </aside>

      <main className="min-w-0">
        <header className="sticky top-0 z-10 flex min-h-16 items-center justify-between gap-3 border-b bg-background/95 px-5 py-3 backdrop-blur sm:px-8">
          <div className="flex min-w-0 items-center gap-3">
            <span className="grid size-9 shrink-0 place-items-center rounded-lg bg-primary text-primary-foreground lg:hidden">
              <ShieldCheck className="size-5" aria-hidden="true" />
            </span>
            <div className="min-w-0">
              <p className="truncate text-base font-semibold">内容管理</p>
              <p className="truncate text-xs text-muted-foreground">当前 revision #{snapshot.active.id}</p>
            </div>
          </div>
          <div className="flex items-center gap-1.5">
            <Tooltip>
              <TooltipTrigger asChild>
                <Button aria-label="刷新内容数据" disabled={dashboardDisabled} onClick={() => void loadSnapshot()} size="icon" variant="ghost">
                  <RefreshCw className={loading ? 'size-4 animate-spin' : 'size-4'} aria-hidden="true" />
                </Button>
              </TooltipTrigger>
              <TooltipContent>刷新</TooltipContent>
            </Tooltip>
            <Tooltip>
              <TooltipTrigger asChild>
                <Button aria-label="退出管理会话" disabled={signingOut || pendingAction !== null || draftDiffLoadingId !== null} onClick={() => void signOut()} size="icon" variant="ghost">
                  <LogOut className="size-4" aria-hidden="true" />
                </Button>
              </TooltipTrigger>
              <TooltipContent>退出</TooltipContent>
            </Tooltip>
          </div>
        </header>

        <div className="space-y-6 px-5 py-5 sm:px-8 sm:py-8">
          {error ? (
            <Alert variant="destructive">
              <AlertTriangle aria-hidden="true" />
              <AlertTitle>读取失败</AlertTitle>
              <AlertDescription>{error}</AlertDescription>
            </Alert>
          ) : null}

          <TabsList className="h-auto w-full justify-start overflow-x-auto rounded-lg bg-muted p-1 lg:hidden">
            {tabs.map(({ label, value }) => (
              <TabsTrigger className="shrink-0" key={value} value={value}>
                {label}
              </TabsTrigger>
            ))}
          </TabsList>

          <TabsContent value="overview">
            <section className="space-y-5">
              <div className="flex flex-wrap items-end justify-between gap-3">
                <div>
                  <p className="text-sm text-muted-foreground">当前内容</p>
                  <h1 className="mt-1 text-2xl font-semibold tracking-normal">
                    {packageLabel(snapshot.active.package_key, snapshot.active.package_revision)}
                  </h1>
                </div>
                <Badge className="h-6 px-2.5" variant="secondary">
                  active revision
                </Badge>
              </div>

              <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
                <MetricCard icon={ClipboardList} label="草稿" value={snapshot.drafts.entries.length} />
                <MetricCard icon={BookOpenCheck} label="revision" value={snapshot.revisions.entries.length} />
                <MetricCard icon={History} label="激活记录" value={snapshot.activations.entries.length} />
                <MetricCard
                  icon={ScrollText}
                  label="审计记录"
                  value={
                    snapshot.operations.entries.length +
                    snapshot.rollbackOperations.entries.length +
                    snapshot.stageOperations.entries.length
                  }
                />
              </div>

              <Card className="rounded-lg shadow-sm">
                <CardHeader className="border-b">
                  <CardTitle>激活内容</CardTitle>
                  <CardDescription>当前 active revision 的脱敏元数据。</CardDescription>
                </CardHeader>
                <CardContent className="grid gap-5 pt-6 sm:grid-cols-2 xl:grid-cols-4">
                  <MetaValue label="revision" value={`#${snapshot.active.id}`} />
                  <MetaValue label="来源格式" value={snapshot.active.source_format.toUpperCase()} />
                  <MetaValue label="内容哈希" mono value={shortHash(snapshot.active.content_hash)} />
                  <MetaValue label="发布时间" value={formatTimestamp(snapshot.active.published_at)} />
                </CardContent>
              </Card>
            </section>
          </TabsContent>

          <TabsContent value="illustrations">
            <PagePanel
              columns={illustrationColumns}
              description="当前编译期 manifest 中的实体插图绑定"
              disabled={dashboardDisabled}
              emptyLabel="暂无插图绑定"
              entries={snapshot.illustrations}
              isLoadingMore={false}
              nextAfterId={null}
              onLoadMore={() => undefined}
              rowKey={(binding) => `${binding.entity_type}:${binding.entity_key}:${binding.media_role}`}
              title="插图绑定"
            />
          </TabsContent>

          <TabsContent value="operations">
            <ContentWritePanel
              activeRevision={snapshot.active}
              disabled={dashboardDisabled}
              drafts={snapshot.drafts.entries}
              feedback={writeFeedback}
              onPublish={publishDraft}
              onRollback={rollbackRevision}
              onStage={stageDraft}
              onValidate={validateDraft}
              pendingAction={pendingAction}
              revisions={snapshot.revisions.entries}
            />
          </TabsContent>

          <TabsContent value="drafts">
            <section className="space-y-8">
              <PagePanel
                columns={draftColumns}
                description="草稿元数据与校验摘要"
                disabled={dashboardDisabled}
                emptyLabel="暂无草稿"
                entries={snapshot.drafts.entries}
                isLoadingMore={loadingMore === 'drafts'}
                nextAfterId={snapshot.drafts.next_after_id}
                onLoadMore={() => void loadMore('drafts')}
                rowKey={(draft) => draft.id}
                title="内容草稿"
              />
              <ContentDraftDiffPreviewPanel
                draft={draftDiffDraft}
                error={draftDiffError}
                loading={draftDiffLoadingId !== null}
                preview={draftDiffPreview}
              />
            </section>
          </TabsContent>

          <TabsContent value="revisions">
            <PagePanel
              columns={revisionColumns}
              description="已发布内容 revision 摘要"
              disabled={dashboardDisabled}
              emptyLabel="暂无已发布 revision"
              entries={snapshot.revisions.entries}
              isLoadingMore={loadingMore === 'revisions'}
              nextAfterId={snapshot.revisions.next_after_id}
              onLoadMore={() => void loadMore('revisions')}
              rowKey={({ revision }) => revision.id}
              title="内容版本"
            />
          </TabsContent>

          <TabsContent value="activations">
            <PagePanel
              columns={activationColumns}
              description="不可变 active revision 历史"
              disabled={dashboardDisabled}
              emptyLabel="暂无激活记录"
              entries={snapshot.activations.entries}
              isLoadingMore={loadingMore === 'activations'}
              nextAfterId={snapshot.activations.next_after_id}
              onLoadMore={() => void loadMore('activations')}
              rowKey={(activation) => activation.id}
              title="激活历史"
            />
          </TabsContent>

          <TabsContent value="audits">
            <section className="space-y-8">
              <PagePanel
                columns={operationColumns}
                description="校验与发布的追加式审计"
                disabled={dashboardDisabled}
                emptyLabel="暂无内容操作审计"
                entries={snapshot.operations.entries}
                isLoadingMore={loadingMore === 'operations'}
                nextAfterId={snapshot.operations.next_after_id}
                onLoadMore={() => void loadMore('operations')}
                rowKey={(operation) => operation.id}
                title="内容操作"
              />
              <Separator />
              <PagePanel
                columns={rollbackColumns}
                description="回滚 activation 的追加式审计"
                disabled={dashboardDisabled}
                emptyLabel="暂无回滚审计"
                entries={snapshot.rollbackOperations.entries}
                isLoadingMore={loadingMore === 'rollbackOperations'}
                nextAfterId={snapshot.rollbackOperations.next_after_id}
                onLoadMore={() => void loadMore('rollbackOperations')}
                rowKey={(operation) => operation.id}
                title="回滚操作"
              />
              <Separator />
              <PagePanel
                columns={stageColumns}
                description="受限内容文件暂存的追加式审计"
                disabled={dashboardDisabled}
                emptyLabel="暂无暂存审计"
                entries={snapshot.stageOperations.entries}
                isLoadingMore={loadingMore === 'stageOperations'}
                nextAfterId={snapshot.stageOperations.next_after_id}
                onLoadMore={() => void loadMore('stageOperations')}
                rowKey={(operation) => operation.id}
                title="暂存操作"
              />
            </section>
          </TabsContent>
        </div>
      </main>
    </Tabs>
  )
}

function MetricCard({
  icon: Icon,
  label,
  value,
}: {
  icon: typeof Database
  label: string
  value: number
}) {
  return (
    <Card className="rounded-lg py-4 shadow-sm">
      <CardContent className="flex items-center justify-between px-4">
        <div>
          <p className="text-sm text-muted-foreground">{label}</p>
          <p className="mt-1 text-2xl font-semibold tabular-nums">{formatNumber(value)}</p>
        </div>
        <span className="grid size-9 place-items-center rounded-md bg-secondary text-secondary-foreground">
          <Icon className="size-4" aria-hidden="true" />
        </span>
      </CardContent>
    </Card>
  )
}

function MetaValue({
  label,
  mono = false,
  value,
}: {
  label: string
  mono?: boolean
  value: string
}) {
  return (
    <div className="min-w-0">
      <p className="text-xs text-muted-foreground">{label}</p>
      <p className={mono ? 'mt-1 truncate font-mono text-sm' : 'mt-1 truncate text-sm font-medium'}>
        {value}
      </p>
    </div>
  )
}
