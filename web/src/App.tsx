import { useEffect, useState } from 'react'

import { ManagementDashboard } from '@/components/management/dashboard'
import { LoginForm } from '@/components/management/login-form'
import { TooltipProvider } from '@/components/ui/tooltip'
import { getSession, type Session } from '@/lib/api'

function App() {
  const [checkingSession, setCheckingSession] = useState(true)
  const [session, setSession] = useState<Session | null>(null)

  useEffect(() => {
    let mounted = true
    void getSession()
      .then((currentSession) => {
        if (mounted) {
          setSession(currentSession)
        }
      })
      .catch(() => {
        if (mounted) {
          setSession(null)
        }
      })
      .finally(() => {
        if (mounted) {
          setCheckingSession(false)
        }
      })

    return () => {
      mounted = false
    }
  }, [])

  return (
    <TooltipProvider>
      {checkingSession ? (
        <div className="min-h-svh bg-background" />
      ) : session ? (
        <ManagementDashboard
          onSessionExpired={() => setSession(null)}
          onSignedOut={() => setSession(null)}
          session={session}
        />
      ) : (
        <LoginForm onAuthenticated={setSession} />
      )}
    </TooltipProvider>
  )
}

export default App
