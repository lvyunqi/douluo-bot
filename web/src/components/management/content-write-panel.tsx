import { type FormEvent, useRef, useState } from 'react'
import { BadgeCheck, Check, FileUp, LoaderCircle, Send, Undo2 } from 'lucide-react'

import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { formatNumber, formatTimestamp, packageLabel, shortHash, statusLabel, statusVariant } from '@/lib/format'
import type { ContentDraft, ContentRevision, ContentRevisionSummary } from '@/lib/api'

export type WriteFeedback = {
  description: string
  kind: 'error' | 'success'
  title: string
}

type ContentWritePanelProps = {
  activeRevision: ContentRevision
  disabled: boolean
  drafts: ContentDraft[]
  feedback: WriteFeedback | null
  onPublish: (draft: ContentDraft) => Promise<boolean>
  onRollback: (revision: ContentRevision) => Promise<boolean>
  onStage: (packageFile: File) => Promise<boolean>
  onValidate: (draft: ContentDraft) => Promise<boolean>
  pendingAction: string | null
  revisions: ContentRevisionSummary[]
}

function EmptyRow({ colSpan, label }: { colSpan: number; label: string }) {
  return (
    <TableRow>
      <TableCell className="h-28 text-center text-muted-foreground" colSpan={colSpan}>
        {label}
      </TableCell>
    </TableRow>
  )
}

function ActionButton({
  children,
  disabled,
  icon: Icon,
  loading,
  onClick,
  variant = 'outline',
}: {
  children: string
  disabled: boolean
  icon: typeof Check
  loading: boolean
  onClick: () => void
  variant?: 'default' | 'destructive' | 'outline'
}) {
  return (
    <Button disabled={disabled} onClick={onClick} size="sm" variant={variant}>
      {loading ? <LoaderCircle className="animate-spin" data-icon="inline-start" aria-hidden="true" /> : <Icon data-icon="inline-start" aria-hidden="true" />}
      {loading ? '处理中' : children}
    </Button>
  )
}

function ValidationSummary({ draft }: { draft: ContentDraft }) {
  if (!draft.validation_errors.length) {
    return <span className="text-muted-foreground">{draft.status === 'validated' ? '通过' : '未校验'}</span>
  }

  return (
    <div className="max-w-[24rem] space-y-1 text-xs text-destructive">
      {draft.validation_errors.slice(0, 2).map((error, index) => (
        <p className="break-words" key={`${draft.id}-${index}`}>
          {error}
        </p>
      ))}
      {draft.validation_errors.length > 2 ? <p>另有 {draft.validation_errors.length - 2} 项错误</p> : null}
    </div>
  )
}

function formatFileSize(byteSize: number) {
  if (byteSize < 1024) return `${byteSize} B`
  if (byteSize < 1024 * 1024) return `${(byteSize / 1024).toFixed(1)} KiB`
  return `${(byteSize / 1024 / 1024).toFixed(2)} MiB`
}

export function ContentWritePanel({
  activeRevision,
  disabled,
  drafts,
  feedback,
  onPublish,
  onRollback,
  onStage,
  onValidate,
  pendingAction,
  revisions,
}: ContentWritePanelProps) {
  const [packageFile, setPackageFile] = useState<File | null>(null)
  const packageInputRef = useRef<HTMLInputElement>(null)
  const [rollbackTarget, setRollbackTarget] = useState<number | null>(null)

  async function submitStage(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (!packageFile || disabled) {
      return
    }
    if (await onStage(packageFile)) {
      setPackageFile(null)
      if (packageInputRef.current) {
        packageInputRef.current.value = ''
      }
    }
  }

  async function confirmRollback(revision: ContentRevision) {
    if (await onRollback(revision)) {
      setRollbackTarget(null)
    }
  }

  return (
    <section className="space-y-6">
      <div className="flex flex-wrap items-end justify-between gap-3">
        <div>
          <h2 className="text-base font-semibold">内容写入</h2>
          <p className="mt-1 text-sm text-muted-foreground">所有操作完成后会重新读取 Store 快照。</p>
        </div>
        <Badge variant="outline">串行保护</Badge>
      </div>

      {feedback ? (
        <Alert variant={feedback.kind === 'error' ? 'destructive' : 'default'}>
          {feedback.kind === 'error' ? <Undo2 aria-hidden="true" /> : <BadgeCheck aria-hidden="true" />}
          <AlertTitle>{feedback.title}</AlertTitle>
          <AlertDescription>{feedback.description}</AlertDescription>
        </Alert>
      ) : null}

      <Card className="rounded-lg shadow-sm">
        <CardHeader className="border-b">
          <CardTitle>上传内容包</CardTitle>
          <CardDescription>选择 UTF-8 JSON 或 TOML 文件并暂存为草稿。</CardDescription>
        </CardHeader>
        <CardContent className="pt-6">
          <form className="grid gap-3 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-end" onSubmit={submitStage}>
            <div className="space-y-2">
              <label className="text-sm font-medium" htmlFor="content-package-file">
                内容包文件
              </label>
              <Input
                disabled={disabled}
                id="content-package-file"
                accept=".json,.toml,application/json,application/toml,text/plain"
                onChange={(event) => setPackageFile(event.target.files?.[0] ?? null)}
                ref={packageInputRef}
                type="file"
              />
              {packageFile ? (
                <p className="break-all text-xs text-muted-foreground">
                  {packageFile.name} · {formatFileSize(packageFile.size)}
                </p>
              ) : null}
            </div>
            <Button disabled={disabled || !packageFile} type="submit">
              {pendingAction === 'stage' ? <LoaderCircle className="animate-spin" data-icon="inline-start" aria-hidden="true" /> : <FileUp data-icon="inline-start" aria-hidden="true" />}
              {pendingAction === 'stage' ? '上传中' : '上传并暂存'}
            </Button>
          </form>
        </CardContent>
      </Card>

      <section className="space-y-3">
        <div className="flex flex-wrap items-end justify-between gap-3">
          <div>
            <h3 className="text-base font-semibold">草稿动作</h3>
            <p className="mt-1 text-sm text-muted-foreground">按草稿状态执行校验和发布。</p>
          </div>
          <Badge variant="outline">{formatNumber(drafts.length)} 条</Badge>
        </div>
        <Card className="rounded-lg py-0 shadow-sm">
          <CardContent className="overflow-x-auto p-0">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>草稿</TableHead>
                  <TableHead>状态</TableHead>
                  <TableHead>内容哈希</TableHead>
                  <TableHead>校验结果</TableHead>
                  <TableHead className="text-right">动作</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {drafts.length ? (
                  drafts.map((draft) => {
                    const validateKey = `validate:${draft.id}`
                    const publishKey = `publish:${draft.id}`
                    return (
                      <TableRow key={draft.id}>
                        <TableCell>
                          <div className="space-y-0.5">
                            <p className="font-medium">{packageLabel(draft.package_key, draft.package_revision)}</p>
                            <p className="font-mono text-xs text-muted-foreground">#{draft.id}</p>
                          </div>
                        </TableCell>
                        <TableCell>
                          <Badge variant={statusVariant(draft.status)}>{statusLabel(draft.status)}</Badge>
                        </TableCell>
                        <TableCell className="font-mono text-xs">{shortHash(draft.content_hash)}</TableCell>
                        <TableCell>
                          <ValidationSummary draft={draft} />
                        </TableCell>
                        <TableCell>
                          <div className="flex min-w-max justify-end gap-2">
                            {draft.status === 'published' ? (
                              <Badge variant="secondary">已发布</Badge>
                            ) : (
                              <ActionButton
                                disabled={disabled}
                                icon={Check}
                                loading={pendingAction === validateKey}
                                onClick={() => void onValidate(draft)}
                              >
                                校验
                              </ActionButton>
                            )}
                            {draft.status === 'validated' ? (
                              <ActionButton
                                disabled={disabled}
                                icon={Send}
                                loading={pendingAction === publishKey}
                                onClick={() => void onPublish(draft)}
                                variant="default"
                              >
                                发布
                              </ActionButton>
                            ) : null}
                          </div>
                        </TableCell>
                      </TableRow>
                    )
                  })
                ) : (
                  <EmptyRow colSpan={5} label="暂无草稿" />
                )}
              </TableBody>
            </Table>
          </CardContent>
        </Card>
      </section>

      <section className="space-y-3">
        <div className="flex flex-wrap items-end justify-between gap-3">
          <div>
            <h3 className="text-base font-semibold">revision 回滚</h3>
            <p className="mt-1 text-sm text-muted-foreground">只追加 activation，不改写历史 revision。</p>
          </div>
          <Badge variant="outline">{formatNumber(revisions.length)} 条</Badge>
        </div>
        <Card className="rounded-lg py-0 shadow-sm">
          <CardContent className="overflow-x-auto p-0">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>revision</TableHead>
                  <TableHead>成员</TableHead>
                  <TableHead>发布时间</TableHead>
                  <TableHead className="text-right">动作</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {revisions.length ? (
                  revisions.map(({ member_count: memberCount, revision }) => {
                    const isActive = revision.id === activeRevision.id
                    const rollbackKey = `rollback:${revision.id}`
                    const isConfirming = rollbackTarget === revision.id
                    return (
                      <TableRow key={revision.id}>
                        <TableCell>
                          <div className="space-y-0.5">
                            <p className="font-medium">{packageLabel(revision.package_key, revision.package_revision)}</p>
                            <p className="font-mono text-xs text-muted-foreground">#{revision.id}</p>
                          </div>
                        </TableCell>
                        <TableCell>{formatNumber(memberCount)}</TableCell>
                        <TableCell>{formatTimestamp(revision.published_at)}</TableCell>
                        <TableCell>
                          <div className="flex min-w-max justify-end gap-2">
                            {isActive ? (
                              <Badge variant="secondary">当前 active</Badge>
                            ) : isConfirming ? (
                              <>
                                <ActionButton
                                  disabled={disabled}
                                  icon={Undo2}
                                  loading={pendingAction === rollbackKey}
                                  onClick={() => void confirmRollback(revision)}
                                  variant="destructive"
                                >
                                  确认回滚
                                </ActionButton>
                                <Button disabled={disabled} onClick={() => setRollbackTarget(null)} size="sm" variant="ghost">
                                  取消
                                </Button>
                              </>
                            ) : (
                              <ActionButton
                                disabled={disabled}
                                icon={Undo2}
                                loading={false}
                                onClick={() => setRollbackTarget(revision.id)}
                              >
                                回滚
                              </ActionButton>
                            )}
                          </div>
                        </TableCell>
                      </TableRow>
                    )
                  })
                ) : (
                  <EmptyRow colSpan={4} label="暂无已发布 revision" />
                )}
              </TableBody>
            </Table>
          </CardContent>
        </Card>
      </section>
    </section>
  )
}
