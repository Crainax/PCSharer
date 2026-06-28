export type AppInfo = {
  deviceId: string;
  deviceName: string;
  inboxDir: string;
  localIp: string | null;
  tcpPort: number;
  udpPort: number;
  trustedDeviceIds: string[];
};

export type DeviceSnapshot = {
  id: string;
  name: string;
  ip: string;
  tcpPort: number;
  platform: string;
  lastSeenMs: number;
  isManual: boolean;
  isTrusted: boolean;
};

export type TransferDirection = "send" | "receive";
export type TransferStatus =
  | "queued"
  | "connecting"
  | "pending"
  | "sending"
  | "receiving"
  | "completed"
  | "failed";

export type TransferSnapshot = {
  id: string;
  direction: TransferDirection;
  peerId: string;
  peerName: string;
  peerIp: string | null;
  peerTcpPort: number | null;
  title: string;
  status: TransferStatus;
  bytesDone: number;
  totalBytes: number;
  fileCount: number;
  currentFile: string | null;
  savedPath: string | null;
  message: string | null;
  sourcePaths: string[];
  canRetry: boolean;
  canAccept: boolean;
  startedAtMs: number;
  updatedAtMs: number;
};
