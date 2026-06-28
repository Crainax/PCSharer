import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import { openPath, revealItemInDir } from "@tauri-apps/plugin-opener";
import {
  Check,
  CheckCircle2,
  CircleAlert,
  Clipboard,
  Clock3,
  FolderOpen,
  HardDriveDownload,
  MonitorUp,
  Network,
  Plus,
  RefreshCw,
  RotateCcw,
  Search,
  Send,
  Settings,
  ShieldCheck,
  ShieldOff,
  UploadCloud,
  Wifi,
  X,
} from "lucide-react";
import type { AppInfo, DeviceSnapshot, TransferSnapshot } from "./types";
import { formatBytes, formatRelativeTime, transferProgress } from "./format";

const statusLabel: Record<TransferSnapshot["status"], string> = {
  queued: "排队中",
  connecting: "连接中",
  pending: "待确认",
  sending: "发送中",
  receiving: "接收中",
  completed: "已完成",
  failed: "失败",
};

function uniquePaths(paths: string[]): string[] {
  return Array.from(new Set(paths.filter(Boolean)));
}

function basename(path: string): string {
  const normalized = path.replace(/\\/g, "/");
  return normalized.slice(normalized.lastIndexOf("/") + 1) || path;
}

function upsertTransfer(
  transfers: TransferSnapshot[],
  next: TransferSnapshot,
): TransferSnapshot[] {
  const existing = transfers.findIndex((transfer) => transfer.id === next.id);

  if (existing === -1) {
    return [next, ...transfers];
  }

  const copy = transfers.slice();
  copy[existing] = next;
  return copy.sort((a, b) => b.updatedAtMs - a.updatedAtMs);
}

export default function App() {
  const [appInfo, setAppInfo] = useState<AppInfo | null>(null);
  const [devices, setDevices] = useState<DeviceSnapshot[]>([]);
  const [transfers, setTransfers] = useState<TransferSnapshot[]>([]);
  const [selectedPaths, setSelectedPaths] = useState<string[]>([]);
  const [selectedDeviceId, setSelectedDeviceId] = useState<string>("");
  const [manualHost, setManualHost] = useState("");
  const [historyQuery, setHistoryQuery] = useState("");
  const [isDragging, setIsDragging] = useState(false);
  const [isSending, setIsSending] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);

  const selectedDevice = useMemo(
    () => devices.find((device) => device.id === selectedDeviceId) ?? null,
    [devices, selectedDeviceId],
  );

  const refresh = useCallback(async () => {
    const [info, nextDevices, nextTransfers] = await Promise.all([
      invoke<AppInfo>("get_app_info"),
      invoke<DeviceSnapshot[]>("list_devices"),
      invoke<TransferSnapshot[]>("list_transfers"),
    ]);

    setAppInfo(info);
    setDevices(nextDevices);
    setTransfers(nextTransfers);
  }, []);

  useEffect(() => {
    const cleanups: UnlistenFn[] = [];

    refresh().catch((error) => setNotice(String(error)));

    listen<DeviceSnapshot[]>("devices-updated", (event) => {
      setDevices(event.payload);
    }).then((unlisten) => cleanups.push(unlisten));

    listen<TransferSnapshot>("transfer-updated", (event) => {
      setTransfers((current) => upsertTransfer(current, event.payload));
    }).then((unlisten) => cleanups.push(unlisten));

    getCurrentWebview()
      .onDragDropEvent((event) => {
        const payload = event.payload;

        if (payload.type === "enter" || payload.type === "over") {
          setIsDragging(true);
          return;
        }

        if (payload.type === "leave") {
          setIsDragging(false);
          return;
        }

        if (payload.type === "drop") {
          setIsDragging(false);
          setSelectedPaths((current) =>
            uniquePaths([...current, ...payload.paths]),
          );
        }
      })
      .then((unlisten) => cleanups.push(unlisten));

    const timer = window.setInterval(() => {
      invoke<DeviceSnapshot[]>("list_devices")
        .then(setDevices)
        .catch(() => undefined);
    }, 3000);

    return () => {
      window.clearInterval(timer);
      cleanups.forEach((cleanup) => cleanup());
    };
  }, [refresh]);

  useEffect(() => {
    if (!selectedDeviceId && devices.length > 0) {
      setSelectedDeviceId(devices[0].id);
    }
  }, [devices, selectedDeviceId]);

  const pickFiles = useCallback(async () => {
    const picked = await open({ multiple: true, directory: false });
    const paths = Array.isArray(picked) ? picked : picked ? [picked] : [];
    setSelectedPaths((current) => uniquePaths([...current, ...paths]));
  }, []);

  const pickDirectory = useCallback(async () => {
    const picked = await open({ multiple: false, directory: true });
    const paths = Array.isArray(picked) ? picked : picked ? [picked] : [];
    setSelectedPaths((current) => uniquePaths([...current, ...paths]));
  }, []);

  const chooseInbox = useCallback(async () => {
    const picked = await open({ multiple: false, directory: true });

    if (!picked || Array.isArray(picked)) {
      return;
    }

    const next = await invoke<AppInfo>("set_inbox_dir", { path: picked });
    setAppInfo(next);
  }, []);

  const addManualDevice = useCallback(async () => {
    const host = manualHost.trim();
    if (!host) {
      return;
    }

    const device = await invoke<DeviceSnapshot>("add_manual_device", {
      host,
      port: null,
    });
    setManualHost("");
    setSelectedDeviceId(device.id);
    await refresh();
  }, [manualHost, refresh]);

  const sendSelected = useCallback(async () => {
    if (!selectedDevice || selectedPaths.length === 0) {
      return;
    }

    setIsSending(true);
    setNotice(null);

    try {
      await invoke<string>("send_paths", {
        paths: selectedPaths,
        targetDeviceId: selectedDevice.id,
      });
      setSelectedPaths([]);
    } catch (error) {
      setNotice(String(error));
    } finally {
      setIsSending(false);
    }
  }, [selectedDevice, selectedPaths]);

  const sendClipboardImage = useCallback(async () => {
    if (!selectedDevice) {
      return;
    }

    setIsSending(true);
    setNotice(null);

    try {
      await invoke<string>("send_clipboard_image", {
        targetDeviceId: selectedDevice.id,
      });
    } catch (error) {
      setNotice(String(error));
    } finally {
      setIsSending(false);
    }
  }, [selectedDevice]);

  const toggleTrusted = useCallback(
    async (device: DeviceSnapshot) => {
      try {
        const next = await invoke<DeviceSnapshot[]>("set_device_trusted", {
          deviceId: device.id,
          trusted: !device.isTrusted,
        });
        setDevices(next);
        const info = await invoke<AppInfo>("get_app_info");
        setAppInfo(info);
      } catch (error) {
        setNotice(String(error));
      }
    },
    [],
  );

  const acceptTransfer = useCallback(
    async (transfer: TransferSnapshot, trustSender: boolean) => {
      try {
        await invoke<void>("accept_incoming_transfer", {
          transferId: transfer.id,
          trustSender,
        });
      } catch (error) {
        setNotice(String(error));
      }
    },
    [],
  );

  const declineTransfer = useCallback(async (transfer: TransferSnapshot) => {
    try {
      await invoke<void>("decline_incoming_transfer", {
        transferId: transfer.id,
      });
    } catch (error) {
      setNotice(String(error));
    }
  }, []);

  const retryTransfer = useCallback(async (transfer: TransferSnapshot) => {
    try {
      await invoke<string>("retry_transfer", { transferId: transfer.id });
    } catch (error) {
      setNotice(String(error));
    }
  }, []);

  const removeSelectedPath = useCallback((path: string) => {
    setSelectedPaths((current) => current.filter((item) => item !== path));
  }, []);

  const openInbox = useCallback(async () => {
    if (appInfo?.inboxDir) {
      await openPath(appInfo.inboxDir);
    }
  }, [appInfo]);

  const normalizedHistoryQuery = historyQuery.trim().toLowerCase();
  const latestTransfers = transfers
    .filter((transfer) => {
      if (!normalizedHistoryQuery) {
        return true;
      }

      return [
        transfer.title,
        transfer.peerName,
        transfer.peerIp ?? "",
        transfer.currentFile ?? "",
        transfer.message ?? "",
        ...transfer.sourcePaths,
      ]
        .join(" ")
        .toLowerCase()
        .includes(normalizedHistoryQuery);
    })
    .slice(0, 30);

  return (
    <main className="app">
      <section className="topbar">
        <div>
          <div className="brand">
            <Network size={22} />
            <h1>PC Sharer</h1>
          </div>
          <p className="muted">
            {appInfo
              ? `${appInfo.deviceName} · ${appInfo.localIp ?? "未识别本机 IP"} · TCP ${appInfo.tcpPort}`
              : "正在启动局域网服务"}
          </p>
        </div>
        <div className="top-actions">
          <button className="button secondary" onClick={refresh} type="button">
            <RefreshCw size={16} />
            刷新
          </button>
          <button className="button secondary" onClick={openInbox} type="button">
            <FolderOpen size={16} />
            收件箱
          </button>
        </div>
      </section>

      {notice ? (
        <div className="notice">
          <CircleAlert size={18} />
          <span>{notice}</span>
          <button aria-label="关闭提示" onClick={() => setNotice(null)} type="button">
            <X size={16} />
          </button>
        </div>
      ) : null}

      <section className="workspace">
        <div className="panel send-panel">
          <div className="panel-heading">
            <div>
              <h2>发送文件</h2>
              <p>拖入文件或文件夹，然后选择目标电脑发送。</p>
            </div>
            <UploadCloud size={24} />
          </div>

          <button
            className={`drop-zone ${isDragging ? "is-dragging" : ""}`}
            onClick={pickFiles}
            type="button"
          >
            <UploadCloud size={34} />
            <span>把文件拖到这里</span>
            <small>也可以点击选择文件</small>
          </button>

          <div className="inline-actions">
            <button className="button secondary" onClick={pickFiles} type="button">
              <Plus size={16} />
              选择文件
            </button>
            <button className="button secondary" onClick={pickDirectory} type="button">
              <FolderOpen size={16} />
              选择文件夹
            </button>
            <button
              className="button secondary"
              disabled={!selectedDevice || isSending}
              onClick={sendClipboardImage}
              type="button"
            >
              <Clipboard size={16} />
              发送剪贴板图片
            </button>
          </div>

          <div className="selection-list">
            {selectedPaths.length === 0 ? (
              <div className="empty-state">还没有选择文件</div>
            ) : (
              selectedPaths.map((path) => (
                <div className="path-row" key={path}>
                  <HardDriveDownload size={16} />
                  <div>
                    <strong>{basename(path)}</strong>
                    <span>{path}</span>
                  </div>
                  <button
                    aria-label="移除"
                    onClick={() => removeSelectedPath(path)}
                    type="button"
                  >
                    <X size={15} />
                  </button>
                </div>
              ))
            )}
          </div>
        </div>

        <div className="panel device-panel">
          <div className="panel-heading">
            <div>
              <h2>目标电脑</h2>
              <p>同一局域网内运行 PC Sharer 后会自动出现。</p>
            </div>
            <Wifi size={24} />
          </div>

          <div className="manual-row">
            <input
              onChange={(event) => setManualHost(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  addManualDevice().catch((error) => setNotice(String(error)));
                }
              }}
              placeholder="手动输入 IP，例如 192.168.1.23"
              value={manualHost}
            />
            <button
              className="icon-button"
              onClick={() => addManualDevice().catch((error) => setNotice(String(error)))}
              title="添加手动 IP"
              type="button"
            >
              <Plus size={18} />
            </button>
          </div>

          <div className="device-list">
            {devices.length === 0 ? (
              <div className="empty-state">暂未发现其他电脑</div>
            ) : (
              devices.map((device) => (
                <div
                  className={`device-row ${
                    selectedDeviceId === device.id ? "is-selected" : ""
                  }`}
                  key={device.id}
                >
                  <button
                    className="device-select"
                    onClick={() => setSelectedDeviceId(device.id)}
                    type="button"
                  >
                    <MonitorUp size={21} />
                    <div>
                      <strong>{device.name}</strong>
                      <span>
                        {device.ip}:{device.tcpPort} · {device.platform}
                        {device.isManual
                          ? " · 手动"
                          : ` · ${formatRelativeTime(device.lastSeenMs)}`}
                        {device.isTrusted ? " · 白名单" : ""}
                      </span>
                    </div>
                  </button>
                  <button
                    className="icon-button trust-button"
                    onClick={() => toggleTrusted(device)}
                    title={device.isTrusted ? "取消自动接收" : "加入自动接收白名单"}
                    type="button"
                  >
                    {device.isTrusted ? (
                      <ShieldCheck size={17} />
                    ) : (
                      <ShieldOff size={17} />
                    )}
                  </button>
                </div>
              ))
            )}
          </div>

          <button
            className="button primary send-button"
            disabled={!selectedDevice || selectedPaths.length === 0 || isSending}
            onClick={sendSelected}
            type="button"
          >
            <Send size={18} />
            {isSending
              ? "正在创建传输"
              : selectedDevice
                ? `发送到 ${selectedDevice.name}`
                : "选择目标电脑"}
          </button>
        </div>
      </section>

      <section className="bottom-grid">
        <div className="panel">
          <div className="panel-heading compact">
            <div>
              <h2>传输记录</h2>
              <p>发送和接收都会在这里显示实时进度。</p>
            </div>
            <Clock3 size={22} />
          </div>

          <div className="search-row">
            <Search size={16} />
            <input
              onChange={(event) => setHistoryQuery(event.target.value)}
              placeholder="搜索文件、电脑或 IP"
              value={historyQuery}
            />
          </div>

          <div className="transfer-list">
            {latestTransfers.length === 0 ? (
              <div className="empty-state">暂无传输</div>
            ) : (
              latestTransfers.map((transfer) => {
                const progress = transferProgress(
                  transfer.bytesDone,
                  transfer.totalBytes,
                );

                return (
                  <div className="transfer-row" key={transfer.id}>
                    <div className="transfer-main">
                      <div className="transfer-title">
                        {transfer.status === "completed" ? (
                          <CheckCircle2 size={18} />
                        ) : transfer.status === "pending" ? (
                          <CircleAlert size={18} />
                        ) : transfer.status === "failed" ? (
                          <CircleAlert size={18} />
                        ) : (
                          <Send size={18} />
                        )}
                        <strong>{transfer.title}</strong>
                        <span>{statusLabel[transfer.status]}</span>
                      </div>
                      <div className="transfer-meta">
                        {transfer.direction === "send" ? "发送到" : "来自"}{" "}
                        {transfer.peerName} · {transfer.fileCount} 个文件 ·{" "}
                        {formatBytes(transfer.bytesDone)} /{" "}
                        {formatBytes(transfer.totalBytes)}
                      </div>
                      {transfer.currentFile ? (
                        <div className="current-file">{transfer.currentFile}</div>
                      ) : null}
                      {transfer.message ? (
                        <div className="transfer-message">{transfer.message}</div>
                      ) : null}
                      <div className="progress-track">
                        <div style={{ width: `${progress}%` }} />
                      </div>
                      {transfer.canAccept ? (
                        <div className="transfer-actions">
                          <button
                            className="button secondary"
                            onClick={() => acceptTransfer(transfer, false)}
                            type="button"
                          >
                            <Check size={16} />
                            接收
                          </button>
                          <button
                            className="button secondary"
                            onClick={() => acceptTransfer(transfer, true)}
                            type="button"
                          >
                            <ShieldCheck size={16} />
                            接收并信任
                          </button>
                          <button
                            className="button secondary"
                            onClick={() => declineTransfer(transfer)}
                            type="button"
                          >
                            <X size={16} />
                            拒绝
                          </button>
                        </div>
                      ) : null}
                    </div>
                    {transfer.canRetry ? (
                      <button
                        className="icon-button"
                        onClick={() => retryTransfer(transfer)}
                        title={
                          transfer.status === "failed"
                            ? "重试并断点续传"
                            : "重新发送"
                        }
                        type="button"
                      >
                        <RotateCcw size={17} />
                      </button>
                    ) : null}
                    {transfer.savedPath ? (
                      <button
                        className="icon-button"
                        onClick={() => revealItemInDir(transfer.savedPath!)}
                        title="在文件夹中显示"
                        type="button"
                      >
                        <FolderOpen size={17} />
                      </button>
                    ) : null}
                  </div>
                );
              })
            )}
          </div>
        </div>

        <div className="panel settings-panel">
          <div className="panel-heading compact">
            <div>
              <h2>本机设置</h2>
              <p>接收文件默认保存到这个目录。</p>
            </div>
            <Settings size={22} />
          </div>

          <div className="setting-row">
            <span>收件箱</span>
            <strong>{appInfo?.inboxDir ?? "加载中"}</strong>
          </div>
          <div className="inline-actions">
            <button className="button secondary" onClick={chooseInbox} type="button">
              <FolderOpen size={16} />
              修改目录
            </button>
            <button className="button secondary" onClick={openInbox} type="button">
              <FolderOpen size={16} />
              打开目录
            </button>
          </div>
        </div>
      </section>
    </main>
  );
}
