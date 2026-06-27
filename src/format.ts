export function formatBytes(value: number): string {
  if (!Number.isFinite(value) || value <= 0) {
    return "0 B";
  }

  const units = ["B", "KB", "MB", "GB", "TB"];
  let current = value;
  let unitIndex = 0;

  while (current >= 1024 && unitIndex < units.length - 1) {
    current /= 1024;
    unitIndex += 1;
  }

  return `${current >= 10 || unitIndex === 0 ? current.toFixed(0) : current.toFixed(1)} ${units[unitIndex]}`;
}

export function formatRelativeTime(timestampMs: number): string {
  const delta = Math.max(0, Date.now() - timestampMs);

  if (delta < 4000) {
    return "刚刚";
  }

  const seconds = Math.floor(delta / 1000);
  if (seconds < 60) {
    return `${seconds} 秒前`;
  }

  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) {
    return `${minutes} 分钟前`;
  }

  const hours = Math.floor(minutes / 60);
  return `${hours} 小时前`;
}

export function transferProgress(done: number, total: number): number {
  if (total <= 0) {
    return 0;
  }

  return Math.max(0, Math.min(100, (done / total) * 100));
}
