export type AppInfo = {
  deviceId: string;
  deviceName: string;
  inboxDir: string;
  localIp: string | null;
  localIps: string[];
  tcpPort: number;
  udpPort: number;
  trustedDeviceIds: string[];
  autoAcceptIncoming: boolean;
  textDraft: string;
  offlinePaths: string[];
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
export type TransferKind = "file" | "text";
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
  kind: TransferKind;
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
  text: string | null;
  sourcePaths: string[];
  canRetry: boolean;
  canAccept: boolean;
  startedAtMs: number;
  updatedAtMs: number;
};

export type BatchSendResult = {
  transferIds: string[];
  queued: boolean;
  targetCount: number;
};

export type OfflineQueueStarted = {
  targetName: string;
  paths: string[];
};
