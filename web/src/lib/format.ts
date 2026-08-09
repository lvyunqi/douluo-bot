const dateFormatter = new Intl.DateTimeFormat('zh-CN', {
  dateStyle: 'medium',
  hour12: false,
  timeStyle: 'medium',
})

const numberFormatter = new Intl.NumberFormat('zh-CN')

export function formatTimestamp(value: number): string {
  if (!Number.isFinite(value) || value <= 0) {
    return '未记录'
  }
  return dateFormatter.format(new Date(value * 1000))
}

export function formatNumber(value: number): string {
  return numberFormatter.format(value)
}

export function shortHash(value: string): string {
  return value.length > 14 ? `${value.slice(0, 10)}...${value.slice(-4)}` : value
}

export function packageLabel(packageKey: string, revision: number): string {
  return `${packageKey} @ ${revision}`
}

export type StatusVariant = 'default' | 'destructive' | 'outline' | 'secondary'

export function statusVariant(value: string): StatusVariant {
  if (value === 'published' || value === 'validated' || value === 'staged') {
    return 'secondary'
  }
  if (value === 'rejected') {
    return 'destructive'
  }
  return 'outline'
}

export function statusLabel(value: string): string {
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
