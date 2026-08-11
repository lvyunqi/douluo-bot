import { type FormEvent, useState } from 'react'
import { ChevronDown, FileSearch, LoaderCircle, UserRoundPlus, X } from 'lucide-react'

import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import {
  ManagementApiError,
  listPlayerStageCandidates,
  type PlayerStageCandidate,
  type PlayerStageCandidates,
} from '@/lib/api'
import { formatNumber, formatTimestamp } from '@/lib/format'

type WriteFeedback = {
  description: string
  kind: 'error' | 'success'
  title: string
}

type PlayerStageConfirmationPanelProps = {
  disabled: boolean
  feedback: WriteFeedback | null
  onConfirm: (stageFile: string, sourcePlayerId: number) => Promise<boolean>
  onSessionExpired: () => void
  pendingAction: string | null
}

function requestMessage(error: unknown): string {
  if (error instanceof ManagementApiError) {
    if (error.status === 401) {
      return '管理会话已失效。'
    }
    if (error.code === 'invalid_player_stage_file') {
      return 'stage 文件路径无效。'
    }
    if (error.code === 'invalid_player_stage') {
      return 'stage 文件不是可确认的 v42.1 资料。'
    }
  }
  return '无法读取 stage 候选。'
}

function EmptyRow() {
  return (
    <TableRow>
      <TableCell className="h-28 text-center text-muted-foreground" colSpan={7}>
        没有可确认候选
      </TableCell>
    </TableRow>
  )
}

export function PlayerStageConfirmationPanel({
  disabled,
  feedback,
  onConfirm,
  onSessionExpired,
  pendingAction,
}: PlayerStageConfirmationPanelProps) {
  const [stageFile, setStageFile] = useState('')
  // 候选列表必须绑定实际读取的文件，避免编辑输入框后确认到另一份 stage。
  const [loadedStageFile, setLoadedStageFile] = useState<string | null>(null)
  const [page, setPage] = useState<PlayerStageCandidates | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)
  const [loadingMore, setLoadingMore] = useState(false)
  const [confirmingId, setConfirmingId] = useState<number | null>(null)

  async function loadCandidates(afterSourcePlayerId?: number) {
    const file = loadedStageFile ?? stageFile.trim()
    if (!file || loading || loadingMore) {
      return
    }
    if (afterSourcePlayerId === undefined) {
      setLoading(true)
    } else {
      setLoadingMore(true)
    }
    setError(null)
    try {
      const next = await listPlayerStageCandidates(file, afterSourcePlayerId)
      setPage((current) => {
        if (afterSourcePlayerId === undefined || !current) {
          return next
        }
        return {
          ...next,
          entries: [...current.entries, ...next.entries],
        }
      })
      if (afterSourcePlayerId === undefined) {
        setLoadedStageFile(file)
      }
    } catch (requestError) {
      if (requestError instanceof ManagementApiError && requestError.status === 401) {
        onSessionExpired()
        return
      }
      setError(requestMessage(requestError))
    } finally {
      setLoading(false)
      setLoadingMore(false)
    }
  }

  async function submitLoad(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (disabled || !stageFile.trim()) {
      return
    }
    setConfirmingId(null)
    await loadCandidates()
  }

  async function confirmCandidate(candidate: PlayerStageCandidate) {
    const file = loadedStageFile
    if (!file || disabled) {
      return
    }
    if (await onConfirm(file, candidate.source_player_id)) {
      setConfirmingId(null)
      await loadCandidates()
    }
  }

  return (
    <section className="space-y-5">
      <div className="flex flex-wrap items-end justify-between gap-3">
        <div>
          <h2 className="text-base font-semibold">玩家导入</h2>
          <p className="mt-1 text-sm text-muted-foreground">单条确认</p>
        </div>
        <Badge variant="outline">受限目录</Badge>
      </div>

      {feedback ? (
        <Alert variant={feedback.kind === 'error' ? 'destructive' : 'default'}>
          <AlertTitle>{feedback.title}</AlertTitle>
          <AlertDescription>{feedback.description}</AlertDescription>
        </Alert>
      ) : null}

      {error ? (
        <Alert variant="destructive">
          <AlertTitle>读取失败</AlertTitle>
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      ) : null}

      <Card className="rounded-lg shadow-sm">
        <CardHeader className="border-b">
          <CardTitle>stage 文件</CardTitle>
          <CardDescription>data_dir 内的 .sqlite 相对路径</CardDescription>
        </CardHeader>
        <CardContent className="pt-6">
          <form className="grid gap-3 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-end" onSubmit={submitLoad}>
            <div className="space-y-2">
              <label className="text-sm font-medium" htmlFor="player-stage-file">
                文件路径
              </label>
              <Input
                disabled={disabled || loading || loadingMore}
                id="player-stage-file"
                onChange={(event) => {
                  // 修改路径后作废当前候选，必须先重新读取再确认。
                  setStageFile(event.target.value)
                  setLoadedStageFile(null)
                  setPage(null)
                  setConfirmingId(null)
                }}
                placeholder="staging/players.sqlite"
                value={stageFile}
              />
            </div>
            <Button disabled={disabled || loading || loadingMore || !stageFile.trim()} type="submit">
              {loading ? (
                <LoaderCircle className="animate-spin" data-icon="inline-start" aria-hidden="true" />
              ) : (
                <FileSearch data-icon="inline-start" aria-hidden="true" />
              )}
              {loading ? '读取中' : '读取候选'}
            </Button>
          </form>
        </CardContent>
      </Card>

      {page ? (
        <section className="space-y-3">
          <div className="flex flex-wrap items-end justify-between gap-3">
            <div>
              <h3 className="text-base font-semibold">候选角色</h3>
              <p className="mt-1 text-sm text-muted-foreground">
                {page.protocol} / {page.account_id} / {page.namespace}
              </p>
            </div>
            <Badge variant="outline">{formatTimestamp(page.staged_at)}</Badge>
          </div>
          <div className="flex flex-wrap gap-2 text-sm text-muted-foreground">
            <span>总计 {formatNumber(page.total_players)}</span>
            <span>可确认 {formatNumber(page.ready_players)}</span>
            <span>已拒绝 {formatNumber(page.rejected_players)}</span>
          </div>
          <Card className="rounded-lg py-0 shadow-sm">
            <CardContent className="overflow-x-auto p-0">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>来源 ID</TableHead>
                    <TableHead>身份</TableHead>
                    <TableHead>角色</TableHead>
                    <TableHead>等级 / 经验</TableHead>
                    <TableHead>生命 / 魂力</TableHead>
                    <TableHead>属性</TableHead>
                    <TableHead className="text-right">操作</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {page.entries.length ? (
                    page.entries.map((candidate) => {
                      const confirming = confirmingId === candidate.source_player_id
                      const confirmingWrite = pendingAction === `player-stage-confirm:${candidate.source_player_id}`
                      return (
                        <TableRow key={candidate.source_player_id}>
                          <TableCell className="font-mono text-xs">{candidate.source_player_id}</TableCell>
                          <TableCell className="font-mono text-xs">{candidate.subject_id}</TableCell>
                          <TableCell>
                            <div className="space-y-0.5">
                              <p className="font-medium">{candidate.name}</p>
                              <p className="text-xs text-muted-foreground">
                                {candidate.gender} / {candidate.life_count} 世
                              </p>
                            </div>
                          </TableCell>
                          <TableCell>
                            <div className="space-y-0.5">
                              <p>Lv.{candidate.level}</p>
                              <p className="font-mono text-xs text-muted-foreground">{formatNumber(candidate.exp)}</p>
                            </div>
                          </TableCell>
                          <TableCell>
                            <div className="space-y-0.5 text-xs">
                              <p>
                                {formatNumber(candidate.hp)} / {formatNumber(candidate.max_hp)}
                              </p>
                              <p className="text-muted-foreground">
                                {formatNumber(candidate.soul_power)} / {formatNumber(candidate.max_soul_power)}
                              </p>
                            </div>
                          </TableCell>
                          <TableCell className="font-mono text-xs text-muted-foreground">
                            力{candidate.strength} 敏{candidate.agility} 精{candidate.spirit} 耐{candidate.endurance}{' '}
                            感{candidate.perception} 运{candidate.luck}
                          </TableCell>
                          <TableCell>
                            <div className="flex min-w-max justify-end gap-2">
                              {confirming ? (
                                <>
                                  <Button
                                    disabled={disabled}
                                    onClick={() => void confirmCandidate(candidate)}
                                    size="sm"
                                    variant="destructive"
                                  >
                                    {confirmingWrite ? (
                                      <LoaderCircle className="animate-spin" data-icon="inline-start" aria-hidden="true" />
                                    ) : (
                                      <UserRoundPlus data-icon="inline-start" aria-hidden="true" />
                                    )}
                                    {confirmingWrite ? '导入中' : '确认导入'}
                                  </Button>
                                  <Tooltip>
                                    <TooltipTrigger asChild>
                                      <Button
                                        aria-label="取消确认导入"
                                        disabled={disabled}
                                        onClick={() => setConfirmingId(null)}
                                        size="icon"
                                        variant="ghost"
                                      >
                                        <X aria-hidden="true" />
                                      </Button>
                                    </TooltipTrigger>
                                    <TooltipContent>取消确认</TooltipContent>
                                  </Tooltip>
                                </>
                              ) : (
                                <Button
                                  disabled={disabled || loading || loadingMore}
                                  onClick={() => setConfirmingId(candidate.source_player_id)}
                                  size="sm"
                                  variant="outline"
                                >
                                  <UserRoundPlus data-icon="inline-start" aria-hidden="true" />
                                  导入
                                </Button>
                              )}
                            </div>
                          </TableCell>
                        </TableRow>
                      )
                    })
                  ) : (
                    <EmptyRow />
                  )}
                </TableBody>
              </Table>
            </CardContent>
          </Card>
          {page.next_after_source_player_id !== null ? (
            <div className="flex justify-center">
              <Button
                disabled={disabled || loading || loadingMore}
                onClick={() => void loadCandidates(page.next_after_source_player_id ?? undefined)}
                variant="outline"
              >
                {loadingMore ? (
                  <LoaderCircle className="animate-spin" data-icon="inline-start" aria-hidden="true" />
                ) : (
                  <ChevronDown data-icon="inline-start" aria-hidden="true" />
                )}
                {loadingMore ? '读取中' : '加载更多'}
              </Button>
            </div>
          ) : null}
        </section>
      ) : null}
    </section>
  )
}
