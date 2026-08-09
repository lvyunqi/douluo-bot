import { type ReactNode, useCallback, useEffect, useMemo, useState } from 'react'
import {
  Activity,
  AlertTriangle,
  BookOpenCheck,
  ChevronDown,
  ClipboardList,
  Database,
  History,
  LogOut,
  RefreshCw,
  ScrollText,
  ShieldCheck,
} from 'lucide-react'

import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
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
  listActivations,
  listDrafts,
  listOperations,
  listRevisions,
  listRollbackOperations,
  listStageOperations,
  logout,
  ManagementApiError,
  type ContentActivation,
  type ContentDraft,
  type ContentOperation,
  type ContentRevision,
  type ContentRevisionSummary,
  type ContentRollbackOperation,
  type ContentStageOperation,
  type CursorPage,
  type Session,
} from '@/lib/api'
import { formatNumber, formatTimestamp, packageLabel, shortHash } from '@/lib/format'

type TabValue = 'overview' | 'drafts' | 'revisions' | 'activations' | 'audits'
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

function statusVariant(value: string): 'default' | 'destructive' | 'outline' | 'secondary' {
  if (value === 'published' || value === 'validated' || value === 'staged') {
    return 'secondary'
  }
  if (value === 'rejected') {
    return 'destructive'
  }
  return 'outline'
}

function statusLabel(value: string): string {
  const labels: Record<string, string> = {
    draft: '草稿',
    published: '已发布',
    rejected: '已拒绝',
    replayed: '重放',
    staged: '已暂存',
    validated: '已校验',
  }
  return labels[value] ?? value
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
  )
}

function PagePanel<T>({
  columns,
  description,
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
          <Button disabled={isLoadingMore} onClick={onLoadMore} variant="outline">
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
  const [signingOut, setSigningOut] = useState(false)

  const loadSnapshot = useCallback(async () => {
    setLoading(true)
    setError(null)
    try {
      const [active, drafts, revisions, activations, operations, rollbackOperations, stageOperations] =
        await Promise.all([
          getActiveRevision(),
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
        operations,
        revisions,
        rollbackOperations,
        stageOperations,
      })
    } catch (requestError) {
      if (requestError instanceof ManagementApiError && requestError.status === 401) {
        onSessionExpired()
        return
      }
      setError(requestMessage(requestError))
    } finally {
      setLoading(false)
    }
  }, [onSessionExpired])

  useEffect(() => {
    void loadSnapshot()
  }, [loadSnapshot])

  async function loadMore(key: PageKey) {
    if (!snapshot || loadingMore) {
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
                <Button aria-label="刷新内容数据" disabled={loading} onClick={() => void loadSnapshot()} size="icon" variant="ghost">
                  <RefreshCw className={loading ? 'size-4 animate-spin' : 'size-4'} aria-hidden="true" />
                </Button>
              </TooltipTrigger>
              <TooltipContent>刷新</TooltipContent>
            </Tooltip>
            <Tooltip>
              <TooltipTrigger asChild>
                <Button aria-label="退出管理会话" disabled={signingOut} onClick={() => void signOut()} size="icon" variant="ghost">
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

          <TabsContent value="drafts">
            <PagePanel
              columns={draftColumns}
              description="草稿元数据与校验摘要"
              emptyLabel="暂无草稿"
              entries={snapshot.drafts.entries}
              isLoadingMore={loadingMore === 'drafts'}
              nextAfterId={snapshot.drafts.next_after_id}
              onLoadMore={() => void loadMore('drafts')}
              rowKey={(draft) => draft.id}
              title="内容草稿"
            />
          </TabsContent>

          <TabsContent value="revisions">
            <PagePanel
              columns={revisionColumns}
              description="已发布内容 revision 摘要"
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
