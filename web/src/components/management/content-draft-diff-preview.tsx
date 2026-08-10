import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import type { ContentDraft, ContentDraftDiffPreview } from '@/lib/api'
import { formatNumber, packageLabel } from '@/lib/format'

type ContentDraftDiffPreviewPanelProps = {
  draft: ContentDraft | null
  error: string | null
  loading: boolean
  preview: ContentDraftDiffPreview | null
}

const memberKindLabels: Record<string, string> = {
  beast: '魂兽',
  'beast-skill': '魂兽技能池',
  curve: '数值曲线',
  effect: '效果',
  item: '物品',
  map: '地图',
  npc: 'NPC',
  quest: '任务',
  ring: '魂环',
  skill: '魂技',
  'starter-skill': '初始魂技',
  state: '状态',
  wuhun: '武魂',
}

function memberKindLabel(memberKind: string): string {
  return memberKindLabels[memberKind] ?? memberKind
}

export function ContentDraftDiffPreviewPanel({
  draft,
  error,
  loading,
  preview,
}: ContentDraftDiffPreviewPanelProps) {
  if (!draft) {
    return null
  }

  return (
    <section className="space-y-3">
      <div>
        <h2 className="text-base font-semibold">草稿差异</h2>
        <p className="mt-1 text-sm text-muted-foreground">
          {packageLabel(draft.package_key, draft.package_revision)}
        </p>
      </div>
      <Card className="rounded-lg shadow-sm">
        <CardHeader className="border-b">
          <CardTitle>成员投影</CardTitle>
          <CardDescription>当前 active revision 与草稿新增成员</CardDescription>
        </CardHeader>
        <CardContent className="space-y-5 pt-6">
          {loading ? (
            <div className="space-y-5">
              <div className="grid gap-3 sm:grid-cols-3">
                {Array.from({ length: 3 }, (_, index) => (
                  <Skeleton className="h-16" key={index} />
                ))}
              </div>
              <Skeleton className="h-24" />
            </div>
          ) : error ? (
            <Alert variant="destructive">
              <AlertTitle>预览不可用</AlertTitle>
              <AlertDescription>{error}</AlertDescription>
            </Alert>
          ) : preview ? (
            <>
              <div className="grid gap-3 sm:grid-cols-3">
                <DiffCount label="当前成员" value={preview.active_member_count} />
                <DiffCount label="新增成员" value={preview.added_members.length} />
                <DiffCount label="预计成员" value={preview.projected_member_count} />
              </div>
              <div className="space-y-3">
                <div className="flex items-center justify-between gap-3">
                  <p className="text-sm font-medium">新增目录成员</p>
                  <Badge variant="outline">{formatNumber(preview.added_members.length)}</Badge>
                </div>
                {preview.added_members.length ? (
                  <div className="flex max-h-72 flex-wrap gap-2 overflow-y-auto pr-1">
                    {preview.added_members.map((member) => (
                      <Badge
                        className="max-w-full gap-1.5 whitespace-normal px-2 py-1 text-left font-normal"
                        key={`${member.member_kind}:${member.member_key}`}
                        variant="secondary"
                      >
                        <span className="shrink-0 text-muted-foreground">
                          {memberKindLabel(member.member_kind)}
                        </span>
                        <span className="break-all font-mono text-xs">{member.member_key}</span>
                      </Badge>
                    ))}
                  </div>
                ) : (
                  <p className="text-sm text-muted-foreground">没有新增目录成员</p>
                )}
              </div>
            </>
          ) : null}
        </CardContent>
      </Card>
    </section>
  )
}

function DiffCount({ label, value }: { label: string; value: number }) {
  return (
    <div className="min-w-0 border-l-2 border-primary/30 pl-3">
      <p className="text-xs text-muted-foreground">{label}</p>
      <p className="mt-1 text-xl font-semibold tabular-nums">{formatNumber(value)}</p>
    </div>
  )
}
