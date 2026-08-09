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
