import { type ChangeEvent, type FormEvent, useState } from 'react'
import { AlertTriangle, CheckCircle2, ImageUp, LoaderCircle, X } from 'lucide-react'

import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import type { IllustrationBinding } from '@/lib/api'
import { formatNumber } from '@/lib/format'

type UploadFeedback = {
  description: string
  kind: 'error' | 'success'
  title: string
}

type DirectIllustrationUploadPanelProps = {
  bindings: IllustrationBinding[]
  disabled: boolean
  feedback: UploadFeedback | null
  onUpload: (assetKey: string, file: File) => Promise<boolean>
  pendingAction: string | null
}

/// 管理端只对现有 manifest 绑定上传文件，避免页面成为任意目录写入入口。
export function DirectIllustrationUploadPanel({
  bindings,
  disabled,
  feedback,
  onUpload,
  pendingAction,
}: DirectIllustrationUploadPanelProps) {
  const [selectedBinding, setSelectedBinding] = useState<IllustrationBinding | null>(null)
  const [selectedFile, setSelectedFile] = useState<File | null>(null)
  const [fileInputKey, setFileInputKey] = useState(0)

  const pendingKey = selectedBinding ? `illustration-upload:${selectedBinding.asset_key}` : null
  const uploading = pendingKey !== null && pendingAction === pendingKey

  function clearSelection() {
    setSelectedBinding(null)
    setSelectedFile(null)
    setFileInputKey((current) => current + 1)
  }

  function selectBinding(binding: IllustrationBinding) {
    setSelectedBinding(binding)
    setSelectedFile(null)
    setFileInputKey((current) => current + 1)
  }

  function selectFile(event: ChangeEvent<HTMLInputElement>) {
    setSelectedFile(event.target.files?.[0] ?? null)
  }

  async function submitUpload(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (!selectedBinding || !selectedFile || disabled) {
      return
    }
    if (await onUpload(selectedBinding.asset_key, selectedFile)) {
      clearSelection()
    }
  }

  return (
    <section className="space-y-5">
      <div className="flex flex-wrap items-end justify-between gap-3">
        <div>
          <h2 className="text-base font-semibold">插图绑定</h2>
          <p className="mt-1 text-sm text-muted-foreground">编译期 manifest</p>
        </div>
        <Badge variant="outline">{formatNumber(bindings.length)} 条</Badge>
      </div>

      {feedback ? (
        <Alert variant={feedback.kind === 'error' ? 'destructive' : 'default'}>
          {feedback.kind === 'error' ? <AlertTriangle aria-hidden="true" /> : <CheckCircle2 aria-hidden="true" />}
          <AlertTitle>{feedback.title}</AlertTitle>
          <AlertDescription>{feedback.description}</AlertDescription>
        </Alert>
      ) : null}

      {selectedBinding ? (
        <Card className="rounded-lg shadow-sm">
          <CardHeader className="border-b">
            <CardTitle>上传插图</CardTitle>
            <CardDescription>保存后重新加载插件生效</CardDescription>
          </CardHeader>
          <CardContent className="pt-6">
            <form className="grid gap-3 lg:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto] lg:items-end" onSubmit={submitUpload}>
              <div className="space-y-2">
                <label className="text-sm font-medium" htmlFor="direct-illustration-asset-key">
                  资源键
                </label>
                <Input
                  className="font-mono text-xs"
                  id="direct-illustration-asset-key"
                  readOnly
                  tabIndex={-1}
                  value={selectedBinding.asset_key}
                />
              </div>
              <div className="space-y-2">
                <label className="text-sm font-medium" htmlFor="direct-illustration-file">
                  图片文件
                </label>
                <Input
                  accept="image/png,image/jpeg,image/webp,image/bmp"
                  disabled={disabled || uploading}
                  id="direct-illustration-file"
                  key={fileInputKey}
                  onChange={selectFile}
                  type="file"
                />
              </div>
              <div className="flex gap-2">
                <Button disabled={disabled || uploading || !selectedFile} type="submit">
                  {uploading ? (
                    <LoaderCircle className="animate-spin" data-icon="inline-start" aria-hidden="true" />
                  ) : (
                    <ImageUp data-icon="inline-start" aria-hidden="true" />
                  )}
                  {uploading ? '保存中' : '保存'}
                </Button>
                <Button disabled={uploading} onClick={clearSelection} size="icon" type="button" variant="outline">
                  <X aria-hidden="true" />
                  <span className="sr-only">取消上传</span>
                </Button>
              </div>
            </form>
          </CardContent>
        </Card>
      ) : null}

      <Card className="rounded-lg py-0 shadow-sm">
        <CardContent className="p-0">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>实体</TableHead>
                <TableHead>用途</TableHead>
                <TableHead>资源键</TableHead>
                <TableHead>说明</TableHead>
                <TableHead>显示尺寸</TableHead>
                <TableHead className="w-12 text-right">操作</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {bindings.length ? bindings.map((binding) => {
                const isSelected = selectedBinding?.asset_key === binding.asset_key
                const isPending = pendingAction === `illustration-upload:${binding.asset_key}`
                return (
                  <TableRow key={`${binding.entity_type}:${binding.entity_key}:${binding.media_role}`}>
                    <TableCell>
                      <div className="space-y-0.5">
                        <p className="font-medium">{binding.entity_key}</p>
                        <p className="text-xs text-muted-foreground">{binding.entity_type}</p>
                      </div>
                    </TableCell>
                    <TableCell>
                      <Badge variant="outline">{binding.media_role}</Badge>
                    </TableCell>
                    <TableCell className="max-w-64 font-mono text-xs">
                      <span className="block truncate">{binding.asset_key}</span>
                    </TableCell>
                    <TableCell className="max-w-48">
                      <span className="block truncate">{binding.alt}</span>
                    </TableCell>
                    <TableCell>{binding.width} × {binding.height}</TableCell>
                    <TableCell className="text-right">
                      <Tooltip>
                        <TooltipTrigger asChild>
                          <Button
                            aria-label={`上传 ${binding.entity_key} 的插图`}
                            disabled={disabled || isPending}
                            onClick={() => selectBinding(binding)}
                            size="icon-sm"
                            variant={isSelected ? 'secondary' : 'ghost'}
                          >
                            {isPending ? <LoaderCircle className="animate-spin" aria-hidden="true" /> : <ImageUp aria-hidden="true" />}
                          </Button>
                        </TooltipTrigger>
                        <TooltipContent>{isPending ? '正在保存' : '上传插图'}</TooltipContent>
                      </Tooltip>
                    </TableCell>
                  </TableRow>
                )
              }) : (
                <TableRow>
                  <TableCell className="h-28 text-center text-muted-foreground" colSpan={6}>
                    暂无插图绑定
                  </TableCell>
                </TableRow>
              )}
            </TableBody>
          </Table>
        </CardContent>
      </Card>
    </section>
  )
}
