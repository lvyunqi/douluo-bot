import { type FormEvent, useState } from 'react'
import { KeyRound, ShieldCheck } from 'lucide-react'

import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { login, ManagementApiError, type Session } from '@/lib/api'

type LoginFormProps = {
  onAuthenticated: (session: Session) => void
}

function loginMessage(error: unknown): string {
  if (error instanceof ManagementApiError) {
    if (error.status === 401) {
      return '管理密钥不正确。'
    }
    if (error.status === 429) {
      return '会话容量已满，请稍后重试。'
    }
  }
  return '管理服务暂时不可用。'
}

export function LoginForm({ onAuthenticated }: LoginFormProps) {
  const [secret, setSecret] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [submitting, setSubmitting] = useState(false)

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (!secret || submitting) {
      return
    }

    setSubmitting(true)
    setError(null)
    try {
      const session = await login(secret)
      setSecret('')
      onAuthenticated(session)
    } catch (requestError) {
      setSecret('')
      setError(loginMessage(requestError))
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <main className="grid min-h-svh bg-background lg:grid-cols-[minmax(0,1fr)_30rem]">
      <section className="hidden border-r border-sidebar-border bg-sidebar px-12 py-12 text-sidebar-foreground lg:flex lg:flex-col lg:justify-between">
        <div className="flex items-center gap-3 text-sm font-medium">
          <span className="grid size-9 place-items-center rounded-lg bg-sidebar-primary text-sidebar-primary-foreground">
            <ShieldCheck className="size-5" aria-hidden="true" />
          </span>
          斗罗内容管理
        </div>
        <div className="max-w-sm text-pretty">
          <p className="text-3xl leading-tight font-semibold tracking-normal text-sidebar-foreground">
            内容 revision
          </p>
          <p className="mt-4 text-sm leading-6 text-sidebar-foreground/70">管理会话</p>
        </div>
        <p className="text-xs text-sidebar-foreground/55">QimenBot 动态插件</p>
      </section>

      <section className="flex items-center justify-center px-5 py-10 sm:px-8">
        <Card className="w-full max-w-sm rounded-lg shadow-sm">
          <CardHeader className="gap-3 border-b">
            <div className="flex items-center gap-3 lg:hidden">
              <span className="grid size-9 place-items-center rounded-lg bg-primary text-primary-foreground">
                <ShieldCheck className="size-5" aria-hidden="true" />
              </span>
              <span className="text-sm font-medium">斗罗内容管理</span>
            </div>
            <CardTitle>管理会话</CardTitle>
          </CardHeader>
          <CardContent className="pt-6">
            <form className="space-y-5" onSubmit={submit}>
              <div className="space-y-2">
                <label className="text-sm font-medium" htmlFor="admin-secret">
                  管理密钥
                </label>
                <Input
                  autoComplete="current-password"
                  disabled={submitting}
                  id="admin-secret"
                  onChange={(event) => setSecret(event.target.value)}
                  placeholder="输入管理密钥"
                  required
                  type="password"
                  value={secret}
                />
              </div>
              {error ? (
                <Alert variant="destructive">
                  <AlertTitle>无法建立会话</AlertTitle>
                  <AlertDescription>{error}</AlertDescription>
                </Alert>
              ) : null}
              <Button className="w-full" disabled={submitting || !secret} type="submit">
                <KeyRound data-icon="inline-start" aria-hidden="true" />
                {submitting ? '验证中' : '登录'}
              </Button>
            </form>
          </CardContent>
        </Card>
      </section>
    </main>
  )
}
