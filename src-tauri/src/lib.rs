use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    env, fs,
    net::{IpAddr, SocketAddr},
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter, State};
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::RwLock,
    time,
};
use uuid::Uuid;
use walkdir::WalkDir;

const APP_PROTOCOL: &str = "pc-sharer-v1";
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const UDP_PORT: u16 = 53342;
const TCP_PORT: u16 = 53343;
const DISCOVERY_INTERVAL_MS: u64 = 1800;
const DEVICE_STALE_MS: u128 = 20_000;
const MAX_METADATA_BYTES: usize = 16 * 1024 * 1024;
const COPY_BUFFER_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    device_id: String,
    device_name: String,
    inbox_dir: String,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TransferDirection {
    Send,
    Receive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TransferStatus {
    Queued,
    Connecting,
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
    title: String,
    status: TransferStatus,
    bytes_done: u64,
    total_bytes: u64,
    file_count: usize,
    current_file: Option<String>,
    saved_path: Option<String>,
    message: Option<String>,
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

#[derive(Debug, Clone)]
struct LocalFileEntry {
    path: PathBuf,
    relative_path: String,
    size: u64,
}

pub struct RuntimeState {
    config_path: PathBuf,
    config: RwLock<Config>,
    devices: RwLock<HashMap<String, DeviceSnapshot>>,
    transfers: RwLock<HashMap<String, TransferSnapshot>>,
}

pub struct AppState {
    inner: Arc<RuntimeState>,
}

impl RuntimeState {
    fn load() -> Result<Self, String> {
        let config_path = config_path()?;
        let config = if config_path.exists() {
            let raw = fs::read_to_string(&config_path)
                .map_err(|error| format!("读取配置失败: {error}"))?;
            serde_json::from_str(&raw).map_err(|error| format!("解析配置失败: {error}"))?
        } else {
            let config = Config {
                device_id: Uuid::new_v4().to_string(),
                device_name: default_device_name(),
                inbox_dir: default_inbox_dir().to_string_lossy().to_string(),
            };
            save_config_file(&config_path, &config)?;
            config
        };

        fs::create_dir_all(&config.inbox_dir)
            .map_err(|error| format!("创建收件箱失败: {error}"))?;

        Ok(Self {
            config_path,
            config: RwLock::new(config),
            devices: RwLock::new(HashMap::new()),
            transfers: RwLock::new(HashMap::new()),
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
async fn add_manual_device(
    host: String,
    port: Option<u16>,
    state: State<'_, AppState>,
) -> Result<DeviceSnapshot, String> {
    let (ip, tcp_port) = parse_manual_target(&host, port.unwrap_or(TCP_PORT))?;
    let device = DeviceSnapshot {
        id: format!("manual:{ip}:{tcp_port}"),
        name: format!("手动 {ip}"),
        ip,
        tcp_port,
        platform: "manual".to_string(),
        last_seen_ms: now_ms(),
        is_manual: true,
    };

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
    let target = {
        let devices = state.inner.devices.read().await;
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
    let transfer_id = Uuid::new_v4().to_string();
    let title = transfer_title(&files);
    let now = now_ms();
    let config = state.inner.config.read().await.clone();

    let transfer = TransferSnapshot {
        id: transfer_id.clone(),
        direction: TransferDirection::Send,
        peer_id: target.id.clone(),
        peer_name: target.name.clone(),
        title,
        status: TransferStatus::Queued,
        bytes_done: 0,
        total_bytes,
        file_count: files.len(),
        current_file: None,
        saved_path: None,
        message: None,
        started_at_ms: now,
        updated_at_ms: now,
    };
    update_transfer(&state.inner, &app, transfer).await;

    let state_for_task = state.inner.clone();
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
    patch_transfer(&state, &app, &transfer_id, |transfer| {
        transfer.status = TransferStatus::Connecting;
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

    patch_transfer(&state, &app, &transfer_id, |transfer| {
        transfer.status = TransferStatus::Sending;
        transfer.message = None;
    })
    .await;

    let mut bytes_done = 0_u64;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];

    for file in files {
        patch_transfer(&state, &app, &transfer_id, |transfer| {
            transfer.current_file = Some(file.relative_path.clone());
        })
        .await;

        let mut input = File::open(&file.path)
            .await
            .map_err(|error| format!("打开文件失败 {}: {error}", file.path.display()))?;

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

            patch_transfer(&state, &app, &transfer_id, |transfer| {
                transfer.bytes_done = bytes_done;
            })
            .await;
        }
    }

    stream
        .shutdown()
        .await
        .map_err(|error| format!("关闭连接失败: {error}"))?;

    patch_transfer(&state, &app, &transfer_id, |transfer| {
        transfer.status = TransferStatus::Completed;
        transfer.bytes_done = total_bytes;
        transfer.current_file = None;
        transfer.message = Some("发送完成".to_string());
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
    let title = request
        .files
        .first()
        .map(|file| {
            if request.files.len() == 1 {
                file.relative_path.clone()
            } else {
                format!("{} 等 {} 个文件", file.relative_path, request.files.len())
            }
        })
        .unwrap_or_else(|| "未命名传输".to_string());

    let transfer = TransferSnapshot {
        id: request.transfer_id.clone(),
        direction: TransferDirection::Receive,
        peer_id: request.sender_id.clone(),
        peer_name: request.sender_name.clone(),
        title,
        status: TransferStatus::Receiving,
        bytes_done: 0,
        total_bytes: request.total_bytes,
        file_count: request.file_count,
        current_file: None,
        saved_path: None,
        message: Some(format!("来自 {}", peer_addr.ip())),
        started_at_ms: now,
        updated_at_ms: now,
    };
    update_transfer(&state, &app, transfer).await;

    let inbox_dir = {
        let config = state.config.read().await;
        PathBuf::from(&config.inbox_dir)
    };
    tokio::fs::create_dir_all(&inbox_dir)
        .await
        .map_err(|error| format!("创建收件箱失败: {error}"))?;

    let mut bytes_done = 0_u64;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    let mut first_saved_path: Option<String> = None;

    for manifest in &request.files {
        let relative_path = sanitize_relative_path(&manifest.relative_path);
        let final_path = unique_destination_path(&inbox_dir.join(relative_path));
        let parent = final_path
            .parent()
            .ok_or_else(|| "无法确定目标文件夹".to_string())?;
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| format!("创建目标文件夹失败: {error}"))?;

        let temp_path = temp_path_for(&final_path, &request.transfer_id);
        let mut output = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&temp_path)
            .await
            .map_err(|error| format!("创建文件失败 {}: {error}", temp_path.display()))?;

        patch_transfer(&state, &app, &request.transfer_id, |transfer| {
            transfer.current_file = Some(manifest.relative_path.clone());
            transfer.message = None;
        })
        .await;

        let mut remaining = manifest.size;
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

            patch_transfer(&state, &app, &request.transfer_id, |transfer| {
                transfer.bytes_done = bytes_done;
            })
            .await;
        }

        output
            .flush()
            .await
            .map_err(|error| format!("刷新文件失败: {error}"))?;
        drop(output);

        tokio::fs::rename(&temp_path, &final_path)
            .await
            .map_err(|error| format!("保存文件失败 {}: {error}", final_path.display()))?;

        if first_saved_path.is_none() {
            first_saved_path = Some(final_path.to_string_lossy().to_string());
        }
    }

    let saved_path = if request.files.len() == 1 {
        first_saved_path
    } else {
        Some(inbox_dir.to_string_lossy().to_string())
    };

    patch_transfer(&state, &app, &request.transfer_id, |transfer| {
        transfer.status = TransferStatus::Completed;
        transfer.bytes_done = request.total_bytes;
        transfer.current_file = None;
        transfer.saved_path = saved_path;
        transfer.message = Some("接收完成".to_string());
    })
    .await;

    Ok(())
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

            let own_id = state.config.read().await.device_id.clone();
            if packet.device_id == own_id {
                continue;
            }

            let device = DeviceSnapshot {
                id: packet.device_id,
                name: packet.device_name,
                ip: address.ip().to_string(),
                tcp_port: packet.tcp_port,
                platform: packet.platform,
                last_seen_ms: now_ms(),
                is_manual: false,
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
) {
    transfer.updated_at_ms = now_ms();
    state
        .transfers
        .write()
        .await
        .insert(transfer.id.clone(), transfer.clone());
    let _ = app.emit("transfer-updated", transfer);
}

async fn patch_transfer<F>(state: &Arc<RuntimeState>, app: &AppHandle, transfer_id: &str, patch: F)
where
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
}

async fn mark_transfer_failed(
    state: &Arc<RuntimeState>,
    app: &AppHandle,
    transfer_id: &str,
    message: String,
) {
    patch_transfer(state, app, transfer_id, |transfer| {
        transfer.status = TransferStatus::Failed;
        transfer.message = Some(message);
    })
    .await;
}

async fn snapshot_devices(state: &Arc<RuntimeState>) -> Vec<DeviceSnapshot> {
    let now = now_ms();
    let mut devices = state.devices.write().await;
    devices.retain(|_, device| {
        device.is_manual || now.saturating_sub(device.last_seen_ms) <= DEVICE_STALE_MS
    });

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
            add_manual_device,
            send_paths
        ])
        .setup(move |app| {
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
