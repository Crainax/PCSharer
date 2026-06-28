use arboard::Clipboard;
use image::{ImageBuffer, Rgba};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    env, fs,
    io::SeekFrom,
    net::{IpAddr, SocketAddr},
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{
    menu::MenuBuilder,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, State, WindowEvent,
};
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::{oneshot, RwLock},
    time,
};
use uuid::Uuid;
use walkdir::WalkDir;

const APP_PROTOCOL: &str = "pc-sharer-v2";
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const UDP_PORT: u16 = 53342;
const TCP_PORT: u16 = 53343;
const DISCOVERY_INTERVAL_MS: u64 = 1800;
const DEVICE_STALE_MS: u128 = 20_000;
const MAX_METADATA_BYTES: usize = 16 * 1024 * 1024;
const COPY_BUFFER_BYTES: usize = 1024 * 1024;
const INCOMING_DECISION_TIMEOUT_SECONDS: u64 = 300;
const MAX_HISTORY_ITEMS: usize = 300;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    device_id: String,
    device_name: String,
    inbox_dir: String,
    #[serde(default)]
    trusted_device_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    device_id: String,
    device_name: String,
    inbox_dir: String,
    local_ip: Option<String>,
    tcp_port: u16,
    udp_port: u16,
    trusted_device_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSnapshot {
    id: String,
    name: String,
    ip: String,
    tcp_port: u16,
    platform: String,
    last_seen_ms: u128,
    is_manual: bool,
    is_trusted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TransferDirection {
    Send,
    Receive,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TransferStatus {
    Queued,
    Connecting,
    Pending,
    Sending,
    Receiving,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferSnapshot {
    id: String,
    direction: TransferDirection,
    peer_id: String,
    peer_name: String,
    #[serde(default)]
    peer_ip: Option<String>,
    #[serde(default)]
    peer_tcp_port: Option<u16>,
    title: String,
    status: TransferStatus,
    bytes_done: u64,
    total_bytes: u64,
    file_count: usize,
    current_file: Option<String>,
    saved_path: Option<String>,
    message: Option<String>,
    #[serde(default)]
    source_paths: Vec<String>,
    #[serde(default)]
    can_retry: bool,
    #[serde(default)]
    can_accept: bool,
    started_at_ms: u128,
    updated_at_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiscoveryPacket {
    protocol: String,
    version: String,
    device_id: String,
    device_name: String,
    tcp_port: u16,
    platform: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileManifest {
    relative_path: String,
    size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TransferRequest {
    protocol: String,
    version: String,
    transfer_id: String,
    sender_id: String,
    sender_name: String,
    file_count: usize,
    total_bytes: u64,
    files: Vec<FileManifest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TransferResponse {
    accepted: bool,
    message: Option<String>,
    offsets: Vec<u64>,
}

#[derive(Debug, Clone)]
struct LocalFileEntry {
    path: PathBuf,
    relative_path: String,
    size: u64,
}

struct PendingIncoming {
    transfer: TransferSnapshot,
    decision_tx: oneshot::Sender<bool>,
}

pub struct RuntimeState {
    config_path: PathBuf,
    history_path: PathBuf,
    config: RwLock<Config>,
    devices: RwLock<HashMap<String, DeviceSnapshot>>,
    transfers: RwLock<HashMap<String, TransferSnapshot>>,
    pending_incoming: RwLock<HashMap<String, PendingIncoming>>,
}

pub struct AppState {
    inner: Arc<RuntimeState>,
}

impl RuntimeState {
    fn load() -> Result<Self, String> {
        let config_path = config_path()?;
        let history_path = config_path
            .parent()
            .ok_or_else(|| "配置路径异常".to_string())?
            .join("history.json");
        let config = if config_path.exists() {
            let raw = fs::read_to_string(&config_path)
                .map_err(|error| format!("读取配置失败: {error}"))?;
            serde_json::from_str(&raw).map_err(|error| format!("解析配置失败: {error}"))?
        } else {
            let config = Config {
                device_id: Uuid::new_v4().to_string(),
                device_name: default_device_name(),
                inbox_dir: default_inbox_dir().to_string_lossy().to_string(),
                trusted_device_ids: Vec::new(),
            };
            save_config_file(&config_path, &config)?;
            config
        };

        fs::create_dir_all(&config.inbox_dir)
            .map_err(|error| format!("创建收件箱失败: {error}"))?;

        let transfers = load_history_file(&history_path);

        Ok(Self {
            config_path,
            history_path,
            config: RwLock::new(config),
            devices: RwLock::new(HashMap::new()),
            transfers: RwLock::new(transfers),
            pending_incoming: RwLock::new(HashMap::new()),
        })
    }
}

#[tauri::command]
async fn get_app_info(state: State<'_, AppState>) -> Result<AppInfo, String> {
    let config = state.inner.config.read().await.clone();
    Ok(AppInfo {
        device_id: config.device_id,
        device_name: config.device_name,
        inbox_dir: config.inbox_dir,
        local_ip: local_ip().map(|ip| ip.to_string()),
        tcp_port: TCP_PORT,
        udp_port: UDP_PORT,
        trusted_device_ids: config.trusted_device_ids,
    })
}

#[tauri::command]
async fn list_devices(state: State<'_, AppState>) -> Result<Vec<DeviceSnapshot>, String> {
    Ok(snapshot_devices(&state.inner).await)
}

#[tauri::command]
async fn list_transfers(state: State<'_, AppState>) -> Result<Vec<TransferSnapshot>, String> {
    let transfers = state.inner.transfers.read().await;
    let mut items: Vec<_> = transfers.values().cloned().collect();
    items.sort_by(|a, b| b.updated_at_ms.cmp(&a.updated_at_ms));
    Ok(items)
}

#[tauri::command]
async fn set_inbox_dir(path: String, state: State<'_, AppState>) -> Result<AppInfo, String> {
    let inbox = PathBuf::from(path);
    fs::create_dir_all(&inbox).map_err(|error| format!("创建收件箱失败: {error}"))?;

    {
        let mut config = state.inner.config.write().await;
        config.inbox_dir = inbox.to_string_lossy().to_string();
        save_config_file(&state.inner.config_path, &config)?;
    }

    get_app_info(state).await
}

#[tauri::command]
async fn set_device_trusted(
    device_id: String,
    trusted: bool,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<Vec<DeviceSnapshot>, String> {
    {
        let mut config = state.inner.config.write().await;
        if trusted {
            if !config.trusted_device_ids.iter().any(|id| id == &device_id) {
                config.trusted_device_ids.push(device_id.clone());
            }
        } else {
            config.trusted_device_ids.retain(|id| id != &device_id);
        }
        save_config_file(&state.inner.config_path, &config)?;
    }

    let devices = snapshot_devices(&state.inner).await;
    let _ = app.emit("devices-updated", devices.clone());
    Ok(devices)
}

#[tauri::command]
async fn add_manual_device(
    host: String,
    port: Option<u16>,
    state: State<'_, AppState>,
) -> Result<DeviceSnapshot, String> {
    let (ip, tcp_port) = parse_manual_target(&host, port.unwrap_or(TCP_PORT))?;
    let config = state.inner.config.read().await;
    let id = format!("manual:{ip}:{tcp_port}");
    let device = DeviceSnapshot {
        id: id.clone(),
        name: format!("手动 {ip}"),
        ip,
        tcp_port,
        platform: "manual".to_string(),
        last_seen_ms: now_ms(),
        is_manual: true,
        is_trusted: config
            .trusted_device_ids
            .iter()
            .any(|trusted| trusted == &id),
    };
    drop(config);

    state
        .inner
        .devices
        .write()
        .await
        .insert(device.id.clone(), device.clone());

    Ok(device)
}

#[tauri::command]
async fn send_paths(
    paths: Vec<String>,
    target_device_id: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<String, String> {
    start_send_paths(
        state.inner.clone(),
        app,
        paths,
        target_device_id,
        None,
        false,
    )
    .await
}

#[tauri::command]
async fn send_clipboard_image(
    target_device_id: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<String, String> {
    let image_path = read_clipboard_image_to_file(&state.inner).await?;
    start_send_paths(
        state.inner.clone(),
        app,
        vec![image_path.to_string_lossy().to_string()],
        target_device_id,
        None,
        false,
    )
    .await
}

#[tauri::command]
async fn retry_transfer(
    transfer_id: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<String, String> {
    let transfer = {
        let transfers = state.inner.transfers.read().await;
        transfers
            .get(&transfer_id)
            .cloned()
            .ok_or_else(|| "找不到历史记录".to_string())?
    };

    if transfer.source_paths.is_empty() {
        return Err("这条记录没有可重发的源文件路径".to_string());
    }

    let target = resolve_target_from_transfer(&state.inner, &transfer).await?;
    state
        .inner
        .devices
        .write()
        .await
        .insert(target.id.clone(), target.clone());
    start_send_paths(
        state.inner.clone(),
        app,
        transfer.source_paths,
        target.id,
        Some(if transfer.status == TransferStatus::Failed {
            transfer.id
        } else {
            Uuid::new_v4().to_string()
        }),
        true,
    )
    .await
}

#[tauri::command]
async fn accept_incoming_transfer(
    transfer_id: String,
    trust_sender: bool,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String> {
    let pending = state
        .inner
        .pending_incoming
        .write()
        .await
        .remove(&transfer_id)
        .ok_or_else(|| "这条接收请求已经失效".to_string())?;

    if trust_sender {
        let mut config = state.inner.config.write().await;
        if !config
            .trusted_device_ids
            .iter()
            .any(|id| id == &pending.transfer.peer_id)
        {
            config
                .trusted_device_ids
                .push(pending.transfer.peer_id.clone());
            save_config_file(&state.inner.config_path, &config)?;
        }
    }

    pending
        .decision_tx
        .send(true)
        .map_err(|_| "发送接收确认失败".to_string())?;
    let devices = snapshot_devices(&state.inner).await;
    let _ = app.emit("devices-updated", devices);
    Ok(())
}

#[tauri::command]
async fn decline_incoming_transfer(
    transfer_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let pending = state
        .inner
        .pending_incoming
        .write()
        .await
        .remove(&transfer_id)
        .ok_or_else(|| "这条接收请求已经失效".to_string())?;

    pending
        .decision_tx
        .send(false)
        .map_err(|_| "发送拒绝确认失败".to_string())
}

async fn start_send_paths(
    state: Arc<RuntimeState>,
    app: AppHandle,
    paths: Vec<String>,
    target_device_id: String,
    transfer_id_override: Option<String>,
    is_retry: bool,
) -> Result<String, String> {
    let target = {
        let devices = state.devices.read().await;
        devices
            .get(&target_device_id)
            .cloned()
            .ok_or_else(|| "目标电脑不在线或不存在".to_string())?
    };

    let files = collect_files(&paths)?;
    if files.is_empty() {
        return Err("没有可发送的文件".to_string());
    }

    let total_bytes = files.iter().map(|file| file.size).sum();
    let transfer_id = transfer_id_override.unwrap_or_else(|| Uuid::new_v4().to_string());
    let title = transfer_title(&files);
    let now = now_ms();
    let config = state.config.read().await.clone();

    let transfer = TransferSnapshot {
        id: transfer_id.clone(),
        direction: TransferDirection::Send,
        peer_id: target.id.clone(),
        peer_name: target.name.clone(),
        peer_ip: Some(target.ip.clone()),
        peer_tcp_port: Some(target.tcp_port),
        title,
        status: TransferStatus::Queued,
        bytes_done: 0,
        total_bytes,
        file_count: files.len(),
        current_file: None,
        saved_path: None,
        message: if is_retry {
            Some("重试传输，接收端会尝试从 .part 文件续传".to_string())
        } else {
            None
        },
        source_paths: paths,
        can_retry: false,
        can_accept: false,
        started_at_ms: now,
        updated_at_ms: now,
    };
    update_transfer(&state, &app, transfer, true).await;

    let state_for_task = state.clone();
    let transfer_id_for_task = transfer_id.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = send_files_task(
            state_for_task.clone(),
            app.clone(),
            transfer_id_for_task.clone(),
            target,
            config,
            files,
            total_bytes,
        )
        .await
        {
            mark_transfer_failed(&state_for_task, &app, &transfer_id_for_task, error).await;
        }
    });

    Ok(transfer_id)
}

async fn send_files_task(
    state: Arc<RuntimeState>,
    app: AppHandle,
    transfer_id: String,
    target: DeviceSnapshot,
    config: Config,
    files: Vec<LocalFileEntry>,
    total_bytes: u64,
) -> Result<(), String> {
    patch_transfer(&state, &app, &transfer_id, true, |transfer| {
        transfer.status = TransferStatus::Connecting;
        transfer.can_retry = false;
        transfer.message = Some(format!("连接 {}:{}", target.ip, target.tcp_port));
    })
    .await;

    let address = format!("{}:{}", target.ip, target.tcp_port);
    let mut stream = TcpStream::connect(&address)
        .await
        .map_err(|error| format!("连接 {address} 失败: {error}"))?;

    let request = TransferRequest {
        protocol: APP_PROTOCOL.to_string(),
        version: APP_VERSION.to_string(),
        transfer_id: transfer_id.clone(),
        sender_id: config.device_id,
        sender_name: config.device_name,
        file_count: files.len(),
        total_bytes,
        files: files
            .iter()
            .map(|file| FileManifest {
                relative_path: file.relative_path.clone(),
                size: file.size,
            })
            .collect(),
    };
    write_json_frame(&mut stream, &request).await?;

    let response: TransferResponse = read_json_frame(&mut stream).await?;
    if !response.accepted {
        return Err(response
            .message
            .unwrap_or_else(|| "接收端拒绝了这次传输".to_string()));
    }

    let offsets = normalized_offsets(&response.offsets, files.len());
    let mut bytes_done = offsets
        .iter()
        .zip(files.iter())
        .map(|(offset, file)| (*offset).min(file.size))
        .sum::<u64>();

    patch_transfer(&state, &app, &transfer_id, true, |transfer| {
        transfer.status = TransferStatus::Sending;
        transfer.bytes_done = bytes_done;
        transfer.message = if bytes_done > 0 {
            Some(format!("已从 {} 继续传输", format_bytes(bytes_done)))
        } else {
            None
        };
    })
    .await;

    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];

    for (index, file) in files.iter().enumerate() {
        let offset = offsets[index].min(file.size);
        if offset >= file.size {
            continue;
        }

        patch_transfer(&state, &app, &transfer_id, false, |transfer| {
            transfer.current_file = Some(file.relative_path.clone());
        })
        .await;

        let mut input = File::open(&file.path)
            .await
            .map_err(|error| format!("打开文件失败 {}: {error}", file.path.display()))?;

        if offset > 0 {
            input
                .seek(SeekFrom::Start(offset))
                .await
                .map_err(|error| format!("定位文件失败 {}: {error}", file.path.display()))?;
        }

        loop {
            let read = input
                .read(&mut buffer)
                .await
                .map_err(|error| format!("读取文件失败 {}: {error}", file.path.display()))?;
            if read == 0 {
                break;
            }

            stream
                .write_all(&buffer[..read])
                .await
                .map_err(|error| format!("发送数据失败: {error}"))?;
            bytes_done += read as u64;

            patch_transfer(&state, &app, &transfer_id, false, |transfer| {
                transfer.bytes_done = bytes_done;
            })
            .await;
        }
    }

    stream
        .shutdown()
        .await
        .map_err(|error| format!("关闭连接失败: {error}"))?;

    patch_transfer(&state, &app, &transfer_id, true, |transfer| {
        transfer.status = TransferStatus::Completed;
        transfer.bytes_done = total_bytes;
        transfer.current_file = None;
        transfer.message = Some("发送完成".to_string());
        transfer.can_retry = true;
    })
    .await;

    Ok(())
}

async fn run_tcp_server(state: Arc<RuntimeState>, app: AppHandle) -> Result<(), String> {
    let listener = TcpListener::bind(("0.0.0.0", TCP_PORT))
        .await
        .map_err(|error| format!("TCP 监听失败: {error}"))?;

    loop {
        let (stream, peer_addr) = listener
            .accept()
            .await
            .map_err(|error| format!("接受连接失败: {error}"))?;
        let task_state = state.clone();
        let task_app = app.clone();

        tauri::async_runtime::spawn(async move {
            if let Err(error) = receive_transfer_task(task_state, task_app, stream, peer_addr).await
            {
                eprintln!("receive transfer failed: {error}");
            }
        });
    }
}

async fn receive_transfer_task(
    state: Arc<RuntimeState>,
    app: AppHandle,
    mut stream: TcpStream,
    peer_addr: SocketAddr,
) -> Result<(), String> {
    let request: TransferRequest = read_json_frame(&mut stream).await?;
    if request.protocol != APP_PROTOCOL {
        return Err("不支持的传输协议".to_string());
    }

    let now = now_ms();
    let title = request_title(&request.files);
    let peer_trusted = {
        let config = state.config.read().await;
        config
            .trusted_device_ids
            .iter()
            .any(|id| id == &request.sender_id)
    };

    let mut transfer = TransferSnapshot {
        id: request.transfer_id.clone(),
        direction: TransferDirection::Receive,
        peer_id: request.sender_id.clone(),
        peer_name: request.sender_name.clone(),
        peer_ip: Some(peer_addr.ip().to_string()),
        peer_tcp_port: Some(TCP_PORT),
        title,
        status: if peer_trusted {
            TransferStatus::Receiving
        } else {
            TransferStatus::Pending
        },
        bytes_done: 0,
        total_bytes: request.total_bytes,
        file_count: request.file_count,
        current_file: None,
        saved_path: None,
        message: Some(if peer_trusted {
            format!("来自 {}，已在白名单中", peer_addr.ip())
        } else {
            format!("来自 {}，等待接收确认", peer_addr.ip())
        }),
        source_paths: Vec::new(),
        can_retry: false,
        can_accept: !peer_trusted,
        started_at_ms: now,
        updated_at_ms: now,
    };
    update_transfer(&state, &app, transfer.clone(), true).await;

    if !peer_trusted {
        let (decision_tx, decision_rx) = oneshot::channel();
        state.pending_incoming.write().await.insert(
            request.transfer_id.clone(),
            PendingIncoming {
                transfer: transfer.clone(),
                decision_tx,
            },
        );

        let decision = time::timeout(
            Duration::from_secs(INCOMING_DECISION_TIMEOUT_SECONDS),
            decision_rx,
        )
        .await;

        let accepted = matches!(decision, Ok(Ok(true)));
        if !accepted {
            state
                .pending_incoming
                .write()
                .await
                .remove(&request.transfer_id);
            let response = TransferResponse {
                accepted: false,
                message: Some("接收端未确认或已拒绝".to_string()),
                offsets: Vec::new(),
            };
            let _ = write_json_frame(&mut stream, &response).await;
            patch_transfer(&state, &app, &request.transfer_id, true, |item| {
                item.status = TransferStatus::Failed;
                item.can_accept = false;
                item.message = Some("已拒绝或确认超时".to_string());
            })
            .await;
            return Ok(());
        }
    }

    let inbox_dir = {
        let config = state.config.read().await;
        PathBuf::from(&config.inbox_dir)
    };
    tokio::fs::create_dir_all(&inbox_dir)
        .await
        .map_err(|error| format!("创建收件箱失败: {error}"))?;

    let receive_plan = build_receive_plan(&inbox_dir, &request).await?;
    let offsets: Vec<u64> = receive_plan.iter().map(|plan| plan.offset).collect();
    let starting_done = offsets
        .iter()
        .zip(request.files.iter())
        .map(|(offset, file)| (*offset).min(file.size))
        .sum::<u64>();
    let response = TransferResponse {
        accepted: true,
        message: None,
        offsets: offsets.clone(),
    };
    write_json_frame(&mut stream, &response).await?;

    transfer.status = TransferStatus::Receiving;
    transfer.bytes_done = starting_done;
    transfer.can_accept = false;
    transfer.message = if starting_done > 0 {
        Some(format!("已从 {} 继续接收", format_bytes(starting_done)))
    } else {
        None
    };
    update_transfer(&state, &app, transfer, true).await;

    let mut bytes_done = starting_done;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    let mut first_saved_path: Option<String> = None;

    for (manifest, plan) in request.files.iter().zip(receive_plan.iter()) {
        if plan.offset >= manifest.size && plan.final_path.exists() {
            if first_saved_path.is_none() {
                first_saved_path = Some(plan.final_path.to_string_lossy().to_string());
            }
            continue;
        }

        let parent = plan
            .final_path
            .parent()
            .ok_or_else(|| "无法确定目标文件夹".to_string())?;
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| format!("创建目标文件夹失败: {error}"))?;

        let mut output = OpenOptions::new()
            .create(true)
            .write(true)
            .append(plan.offset > 0)
            .truncate(plan.offset == 0)
            .open(&plan.temp_path)
            .await
            .map_err(|error| format!("创建文件失败 {}: {error}", plan.temp_path.display()))?;

        patch_transfer(&state, &app, &request.transfer_id, false, |item| {
            item.current_file = Some(manifest.relative_path.clone());
            item.message = None;
        })
        .await;

        let mut remaining = manifest.size.saturating_sub(plan.offset);
        while remaining > 0 {
            let chunk_size = remaining.min(buffer.len() as u64) as usize;
            stream
                .read_exact(&mut buffer[..chunk_size])
                .await
                .map_err(|error| format!("接收数据失败: {error}"))?;
            output
                .write_all(&buffer[..chunk_size])
                .await
                .map_err(|error| format!("写入文件失败: {error}"))?;

            remaining -= chunk_size as u64;
            bytes_done += chunk_size as u64;

            patch_transfer(&state, &app, &request.transfer_id, false, |item| {
                item.bytes_done = bytes_done;
            })
            .await;
        }

        output
            .flush()
            .await
            .map_err(|error| format!("刷新文件失败: {error}"))?;
        drop(output);

        tokio::fs::rename(&plan.temp_path, &plan.final_path)
            .await
            .map_err(|error| format!("保存文件失败 {}: {error}", plan.final_path.display()))?;

        if first_saved_path.is_none() {
            first_saved_path = Some(plan.final_path.to_string_lossy().to_string());
        }
    }

    let saved_path = if request.files.len() == 1 {
        first_saved_path
    } else {
        Some(inbox_dir.to_string_lossy().to_string())
    };

    patch_transfer(&state, &app, &request.transfer_id, true, |item| {
        item.status = TransferStatus::Completed;
        item.bytes_done = request.total_bytes;
        item.current_file = None;
        item.saved_path = saved_path;
        item.message = Some("接收完成".to_string());
    })
    .await;

    Ok(())
}

#[derive(Debug, Clone)]
struct ReceivePlan {
    final_path: PathBuf,
    temp_path: PathBuf,
    offset: u64,
}

async fn build_receive_plan(
    inbox_dir: &Path,
    request: &TransferRequest,
) -> Result<Vec<ReceivePlan>, String> {
    let mut plan = Vec::with_capacity(request.files.len());

    for manifest in &request.files {
        let relative_path = sanitize_relative_path(&manifest.relative_path);
        let preferred_final = inbox_dir.join(relative_path);
        let final_path = if preferred_final.exists()
            && fs::metadata(&preferred_final)
                .map(|metadata| metadata.len() == manifest.size)
                .unwrap_or(false)
        {
            preferred_final
        } else if preferred_final.exists() {
            unique_destination_path(&preferred_final)
        } else {
            preferred_final
        };
        let temp_path = temp_path_for(&final_path, &request.transfer_id);

        let offset = if final_path.exists() {
            let len = fs::metadata(&final_path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            if len == manifest.size {
                len
            } else {
                0
            }
        } else if temp_path.exists() {
            let len = fs::metadata(&temp_path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            if len <= manifest.size {
                len
            } else {
                0
            }
        } else {
            0
        };

        plan.push(ReceivePlan {
            final_path,
            temp_path,
            offset,
        });
    }

    Ok(plan)
}

async fn run_discovery(state: Arc<RuntimeState>, app: AppHandle) -> Result<(), String> {
    let socket = Arc::new(
        UdpSocket::bind(("0.0.0.0", UDP_PORT))
            .await
            .map_err(|error| format!("UDP 监听失败: {error}"))?,
    );
    socket
        .set_broadcast(true)
        .map_err(|error| format!("启用 UDP 广播失败: {error}"))?;

    let sender_socket = socket.clone();
    let sender_state = state.clone();
    tauri::async_runtime::spawn(async move {
        let target = format!("255.255.255.255:{UDP_PORT}");
        let mut interval = time::interval(Duration::from_millis(DISCOVERY_INTERVAL_MS));

        loop {
            interval.tick().await;

            let config = sender_state.config.read().await.clone();
            let packet = DiscoveryPacket {
                protocol: APP_PROTOCOL.to_string(),
                version: APP_VERSION.to_string(),
                device_id: config.device_id,
                device_name: config.device_name,
                tcp_port: TCP_PORT,
                platform: env::consts::OS.to_string(),
            };

            if let Ok(payload) = serde_json::to_vec(&packet) {
                let _ = sender_socket.send_to(&payload, &target).await;
            }
        }
    });

    let mut buffer = vec![0_u8; 4096];
    loop {
        let (size, address) = socket
            .recv_from(&mut buffer)
            .await
            .map_err(|error| format!("接收 UDP 广播失败: {error}"))?;

        if let Ok(packet) = serde_json::from_slice::<DiscoveryPacket>(&buffer[..size]) {
            if packet.protocol != APP_PROTOCOL {
                continue;
            }

            let config = state.config.read().await.clone();
            if packet.device_id == config.device_id {
                continue;
            }

            let device_id = packet.device_id.clone();
            let device = DeviceSnapshot {
                id: device_id.clone(),
                name: packet.device_name,
                ip: address.ip().to_string(),
                tcp_port: packet.tcp_port,
                platform: packet.platform,
                last_seen_ms: now_ms(),
                is_manual: false,
                is_trusted: config.trusted_device_ids.iter().any(|id| id == &device_id),
            };

            state
                .devices
                .write()
                .await
                .insert(device.id.clone(), device);

            let devices = snapshot_devices(&state).await;
            let _ = app.emit("devices-updated", devices);
        }
    }
}

async fn write_json_frame<T: Serialize>(stream: &mut TcpStream, value: &T) -> Result<(), String> {
    let payload =
        serde_json::to_vec(value).map_err(|error| format!("序列化元数据失败: {error}"))?;
    if payload.len() > MAX_METADATA_BYTES {
        return Err("传输元数据过大".to_string());
    }

    let len = (payload.len() as u32).to_be_bytes();
    stream
        .write_all(&len)
        .await
        .map_err(|error| format!("发送元数据长度失败: {error}"))?;
    stream
        .write_all(&payload)
        .await
        .map_err(|error| format!("发送元数据失败: {error}"))?;

    Ok(())
}

async fn read_json_frame<T: for<'de> Deserialize<'de>>(
    stream: &mut TcpStream,
) -> Result<T, String> {
    let mut len_bytes = [0_u8; 4];
    stream
        .read_exact(&mut len_bytes)
        .await
        .map_err(|error| format!("读取元数据长度失败: {error}"))?;
    let len = u32::from_be_bytes(len_bytes) as usize;
    if len == 0 || len > MAX_METADATA_BYTES {
        return Err("传输元数据长度异常".to_string());
    }

    let mut payload = vec![0_u8; len];
    stream
        .read_exact(&mut payload)
        .await
        .map_err(|error| format!("读取元数据失败: {error}"))?;
    serde_json::from_slice(&payload).map_err(|error| format!("解析元数据失败: {error}"))
}

async fn update_transfer(
    state: &Arc<RuntimeState>,
    app: &AppHandle,
    mut transfer: TransferSnapshot,
    persist: bool,
) {
    transfer.updated_at_ms = now_ms();
    state
        .transfers
        .write()
        .await
        .insert(transfer.id.clone(), transfer.clone());
    let _ = app.emit("transfer-updated", transfer);

    if persist {
        persist_history(state).await;
    }
}

async fn patch_transfer<F>(
    state: &Arc<RuntimeState>,
    app: &AppHandle,
    transfer_id: &str,
    persist: bool,
    patch: F,
) where
    F: FnOnce(&mut TransferSnapshot),
{
    let updated = {
        let mut transfers = state.transfers.write().await;
        let Some(transfer) = transfers.get_mut(transfer_id) else {
            return;
        };
        patch(transfer);
        transfer.updated_at_ms = now_ms();
        transfer.clone()
    };

    let _ = app.emit("transfer-updated", updated);

    if persist {
        persist_history(state).await;
    }
}

async fn mark_transfer_failed(
    state: &Arc<RuntimeState>,
    app: &AppHandle,
    transfer_id: &str,
    message: String,
) {
    patch_transfer(state, app, transfer_id, true, |transfer| {
        transfer.status = TransferStatus::Failed;
        transfer.message = Some(message);
        transfer.current_file = None;
        transfer.can_retry = matches!(transfer.direction, TransferDirection::Send)
            && !transfer.source_paths.is_empty();
    })
    .await;
}

async fn persist_history(state: &Arc<RuntimeState>) {
    let mut items: Vec<_> = {
        let transfers = state.transfers.read().await;
        transfers.values().cloned().collect()
    };
    items.sort_by(|a, b| b.updated_at_ms.cmp(&a.updated_at_ms));
    items.truncate(MAX_HISTORY_ITEMS);

    if let Ok(raw) = serde_json::to_string_pretty(&items) {
        let _ = tokio::fs::write(&state.history_path, raw).await;
    }
}

async fn snapshot_devices(state: &Arc<RuntimeState>) -> Vec<DeviceSnapshot> {
    let now = now_ms();
    let trusted = state.config.read().await.trusted_device_ids.clone();
    let mut devices = state.devices.write().await;
    devices.retain(|_, device| {
        device.is_manual || now.saturating_sub(device.last_seen_ms) <= DEVICE_STALE_MS
    });

    for device in devices.values_mut() {
        device.is_trusted = trusted.iter().any(|id| id == &device.id);
    }

    let mut items: Vec<_> = devices.values().cloned().collect();
    items.sort_by(|a, b| {
        a.is_manual
            .cmp(&b.is_manual)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    items
}

fn collect_files(paths: &[String]) -> Result<Vec<LocalFileEntry>, String> {
    let mut entries = Vec::new();

    for raw in paths {
        let path = PathBuf::from(raw);
        if !path.exists() {
            return Err(format!("路径不存在: {}", path.display()));
        }

        if path.is_file() {
            let metadata =
                fs::metadata(&path).map_err(|error| format!("读取文件信息失败: {error}"))?;
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| format!("无法读取文件名: {}", path.display()))?
                .to_string();
            entries.push(LocalFileEntry {
                path,
                relative_path: name,
                size: metadata.len(),
            });
            continue;
        }

        if path.is_dir() {
            let root_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("folder")
                .to_string();

            for item in WalkDir::new(&path).follow_links(false) {
                let item = item.map_err(|error| format!("遍历文件夹失败: {error}"))?;
                if !item.file_type().is_file() {
                    continue;
                }

                let item_path = item.path().to_path_buf();
                let metadata = item
                    .metadata()
                    .map_err(|error| format!("读取文件信息失败: {error}"))?;
                let inside = item_path
                    .strip_prefix(&path)
                    .map_err(|error| format!("生成相对路径失败: {error}"))?;
                let relative = Path::new(&root_name)
                    .join(inside)
                    .to_string_lossy()
                    .replace('\\', "/");

                entries.push(LocalFileEntry {
                    path: item_path,
                    relative_path: relative,
                    size: metadata.len(),
                });
            }
        }
    }

    entries.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    Ok(entries)
}

fn transfer_title(files: &[LocalFileEntry]) -> String {
    match files {
        [] => "空传输".to_string(),
        [file] => file.relative_path.clone(),
        [first, ..] => format!("{} 等 {} 个文件", first.relative_path, files.len()),
    }
}

fn request_title(files: &[FileManifest]) -> String {
    match files {
        [] => "未命名传输".to_string(),
        [file] => file.relative_path.clone(),
        [first, ..] => format!("{} 等 {} 个文件", first.relative_path, files.len()),
    }
}

fn normalized_offsets(offsets: &[u64], len: usize) -> Vec<u64> {
    let mut output = offsets.to_vec();
    output.resize(len, 0);
    output
}

async fn resolve_target_from_transfer(
    state: &Arc<RuntimeState>,
    transfer: &TransferSnapshot,
) -> Result<DeviceSnapshot, String> {
    let devices = state.devices.read().await;
    if let Some(device) = devices.get(&transfer.peer_id) {
        return Ok(device.clone());
    }

    let Some(ip) = transfer.peer_ip.clone() else {
        return Err("目标电脑不在线，也没有历史 IP 可用".to_string());
    };
    let tcp_port = transfer.peer_tcp_port.unwrap_or(TCP_PORT);
    Ok(DeviceSnapshot {
        id: transfer.peer_id.clone(),
        name: transfer.peer_name.clone(),
        ip,
        tcp_port,
        platform: "history".to_string(),
        last_seen_ms: now_ms(),
        is_manual: true,
        is_trusted: false,
    })
}

fn parse_manual_target(raw: &str, default_port: u16) -> Result<(String, u16), String> {
    let value = raw.trim();
    if value.is_empty() {
        return Err("请输入目标 IP".to_string());
    }

    if let Some((host, port)) = value.rsplit_once(':') {
        if !host.contains(':') {
            let parsed_port = port
                .parse::<u16>()
                .map_err(|_| "端口格式不正确".to_string())?;
            return Ok((host.trim().to_string(), parsed_port));
        }
    }

    Ok((value.to_string(), default_port))
}

fn sanitize_relative_path(relative_path: &str) -> PathBuf {
    let normalized = relative_path.replace('\\', "/");
    let mut output = PathBuf::new();

    for part in normalized.split('/') {
        let component = Path::new(part).components().next();
        if let Some(Component::Normal(value)) = component {
            output.push(value);
        }
    }

    if output.as_os_str().is_empty() {
        output.push("received-file");
    }

    output
}

fn unique_destination_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }

    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("file");
    let extension = path.extension().and_then(|value| value.to_str());

    for index in 1..10_000 {
        let file_name = match extension {
            Some(extension) if !extension.is_empty() => {
                format!("{stem} ({index}).{extension}")
            }
            _ => format!("{stem} ({index})"),
        };
        let candidate = parent.join(file_name);
        if !candidate.exists() {
            return candidate;
        }
    }

    path.to_path_buf()
}

fn temp_path_for(final_path: &Path, transfer_id: &str) -> PathBuf {
    let file_name = final_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("received-file");
    final_path.with_file_name(format!("{file_name}.{transfer_id}.part"))
}

async fn read_clipboard_image_to_file(state: &Arc<RuntimeState>) -> Result<PathBuf, String> {
    let image = tokio::task::spawn_blocking(|| -> Result<arboard::ImageData<'static>, String> {
        let mut clipboard = Clipboard::new().map_err(|error| format!("打开剪贴板失败: {error}"))?;
        clipboard
            .get_image()
            .map_err(|error| format!("剪贴板里没有可读取的图片: {error}"))
    })
    .await
    .map_err(|error| format!("读取剪贴板任务失败: {error}"))??;

    let width = image.width as u32;
    let height = image.height as u32;
    let rgba = ImageBuffer::<Rgba<u8>, Vec<u8>>::from_raw(width, height, image.bytes.into_owned())
        .ok_or_else(|| "剪贴板图片像素格式异常".to_string())?;
    let dir = state
        .config_path
        .parent()
        .ok_or_else(|| "配置路径异常".to_string())?
        .join("clipboard");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|error| format!("创建剪贴板缓存目录失败: {error}"))?;
    let path = dir.join(format!("clipboard-{}.png", now_ms()));
    let write_path = path.clone();

    tokio::task::spawn_blocking(move || {
        rgba.save(&write_path)
            .map_err(|error| format!("保存剪贴板图片失败: {error}"))
    })
    .await
    .map_err(|error| format!("保存剪贴板图片任务失败: {error}"))??;

    Ok(path)
}

fn config_path() -> Result<PathBuf, String> {
    let base = if cfg!(target_os = "windows") {
        env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| env::var_os("APPDATA").map(PathBuf::from))
            .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
    } else if cfg!(target_os = "macos") {
        env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
        })
    } else {
        env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
    }
    .ok_or_else(|| "无法确定配置目录".to_string())?;

    let dir = base.join("PCSharer");
    fs::create_dir_all(&dir).map_err(|error| format!("创建配置目录失败: {error}"))?;
    Ok(dir.join("config.json"))
}

fn save_config_file(path: &Path, config: &Config) -> Result<(), String> {
    let parent = path.parent().ok_or_else(|| "配置路径异常".to_string())?;
    fs::create_dir_all(parent).map_err(|error| format!("创建配置目录失败: {error}"))?;
    let raw =
        serde_json::to_string_pretty(config).map_err(|error| format!("序列化配置失败: {error}"))?;
    fs::write(path, raw).map_err(|error| format!("保存配置失败: {error}"))
}

fn load_history_file(path: &Path) -> HashMap<String, TransferSnapshot> {
    let Ok(raw) = fs::read_to_string(path) else {
        return HashMap::new();
    };
    let Ok(items) = serde_json::from_str::<Vec<TransferSnapshot>>(&raw) else {
        return HashMap::new();
    };

    items
        .into_iter()
        .map(|item| (item.id.clone(), item))
        .collect()
}

fn default_device_name() -> String {
    env::var("COMPUTERNAME")
        .or_else(|_| env::var("HOSTNAME"))
        .map(|name| format!("{name}"))
        .unwrap_or_else(|_| "PC Sharer".to_string())
}

fn default_inbox_dir() -> PathBuf {
    if cfg!(target_os = "windows") {
        if let Some(user_profile) = env::var_os("USERPROFILE") {
            return PathBuf::from(user_profile)
                .join("Downloads")
                .join("PCSharer");
        }
    }

    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home).join("Downloads").join("PCSharer");
    }

    env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("PCSharer-Inbox")
}

fn local_ip() -> Option<IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    socket.local_addr().ok().map(|addr| addr.ip())
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn format_bytes(value: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut current = value as f64;
    let mut unit_index = 0;

    while current >= 1024.0 && unit_index < UNITS.len() - 1 {
        current /= 1024.0;
        unit_index += 1;
    }

    if unit_index == 0 || current >= 10.0 {
        format!("{current:.0} {}", UNITS[unit_index])
    } else {
        format!("{current:.1} {}", UNITS[unit_index])
    }
}

fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    let show = MenuBuilder::new(app)
        .text("show", "显示窗口")
        .separator()
        .quit()
        .build()?;
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/icon.png"))?;

    TrayIconBuilder::new()
        .tooltip("PC Sharer")
        .icon(icon)
        .menu(&show)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| {
            if event.id().as_ref() == "show" {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                if let Some(window) = tray.app_handle().get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
        })
        .build(app)?;

    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let runtime_state = Arc::new(RuntimeState::load().expect("failed to initialize PC Sharer"));
    let setup_state = runtime_state.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState {
            inner: runtime_state,
        })
        .invoke_handler(tauri::generate_handler![
            get_app_info,
            list_devices,
            list_transfers,
            set_inbox_dir,
            set_device_trusted,
            add_manual_device,
            send_paths,
            send_clipboard_image,
            retry_transfer,
            accept_incoming_transfer,
            decline_incoming_transfer
        ])
        .setup(move |app| {
            setup_tray(app)?;

            if let Some(window) = app.get_webview_window("main") {
                let close_window = window.clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = close_window.hide();
                    }
                });
            }

            let handle = app.handle().clone();

            let tcp_state = setup_state.clone();
            let tcp_handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(error) = run_tcp_server(tcp_state, tcp_handle).await {
                    eprintln!("{error}");
                }
            });

            let discovery_state = setup_state.clone();
            let discovery_handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(error) = run_discovery(discovery_state, discovery_handle).await {
                    eprintln!("{error}");
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running PC Sharer");
}
