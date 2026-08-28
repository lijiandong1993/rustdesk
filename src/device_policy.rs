// 服务端下发策略（全局管控）——定制客户端模块。
//
// 功能：启动时从边缘 Agent 拉取「该设备策略」并应用：
//   ① 远程连接密码下发（复用官方 set_permanent_password 链路）
//   ② 强制开机自启（平台原生实现）
//   ③ 卸载保护密码 / 打开客户端密码 / 关机保护标记（写入本地选项，供 core_main/connection/UI 读取）
//   ④ 在线心跳直报（后台线程定时 POST）
//
// 配置注入：命令行 `--agent-url <url> --agent-token <token>` 或本地选项 agent_url/agent_token。
// 失败兜底：网络不可达/服务端无策略时静默，不影响客户端本地既有配置。

use std::thread;
use std::time::Duration;

use crate::ui_interface::{get_local_option, set_local_option};

pub const AGENT_URL: &str = "agent_url";
pub const AGENT_TOKEN: &str = "agent_token";

/// 启动时同步策略（同步调用；未配置 agent-url/token 或拉取失败时静默返回）。
pub fn sync() {
    let base = get_local_option(AGENT_URL.to_string()).trim().trim_end_matches('/').to_owned();
    let token = get_local_option(AGENT_TOKEN.to_string());
    if base.is_empty() || token.is_empty() {
        return;
    }
    let id = crate::ipc::get_id();
    let req_url = format!("{}/api/device/policy?device_id={}&token={}", base, id, token);
    let body = match reqwest::blocking::get(&req_url) {
        Ok(resp) if resp.status().is_success() => resp.text().unwrap_or_default(),
        _ => return,
    };
    let value: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => return,
    };
    // ① 远程连接密码下发（复用官方链路，被控端本地校验）
    if let Some(pwd) = value.get("password").and_then(|x| x.as_str()) {
        if !pwd.is_empty() {
            let _ = crate::ipc::set_permanent_password(pwd.to_owned());
            hbb_common::log::info!("[EDGE] 已应用服务端下发的远程连接密码");
        }
    }
    // ② 策略字段应用
    if let Some(p) = value.get("policy") {
        if let Some(b) = p.get("force_auto_start").and_then(|x| x.as_bool()) {
            set_local_option("force_auto_start".to_string(), if b { "Y" } else { "" }.to_owned());
            if b {
                enable_auto_start();
            }
        }
        if let Some(s) = p.get("uninstall_password").and_then(|x| x.as_str()) {
            set_local_option("uninstall_password".to_string(), s.to_owned());
        }
        if let Some(s) = p.get("ui_password").and_then(|x| x.as_str()) {
            set_local_option("ui_password".to_string(), s.to_owned());
        }
        if let Some(b) = p.get("remote_shutdown_protect").and_then(|x| x.as_bool()) {
            set_local_option("remote_shutdown_protect".to_string(), if b { "Y" } else { "" }.to_owned());
        }
        let hb = p
            .get("heartbeat_interval")
            .and_then(|x| x.as_u64())
            .unwrap_or(30)
            .clamp(15, 300);
        set_local_option("heartbeat_interval".to_string(), hb.to_string());
        // ③ 在线心跳直报
        spawn_heartbeat(base, id, token, hb);
    }
}

/// 在线心跳直报线程（失败静默重试）
fn spawn_heartbeat(base: String, id: String, token: String, interval: u64) {
    thread::spawn(move || {
        let url = format!("{}/api/device/heartbeat?device_id={}&token={}", base, id, token);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        loop {
            let _ = client.post(&url).send();
            thread::sleep(Duration::from_secs(interval));
        }
    });
}

/// 强制开机自启（平台原生实现）
#[cfg(target_os = "windows")]
fn enable_auto_start() {
    if let Ok(exe) = std::env::current_exe() {
        let cmd = format!("\"{}\"", exe.display());
        let _ = std::process::Command::new("reg")
            .args([
                "add",
                "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
                "/v",
                "RustDeskEdge",
                "/t",
                "REG_SZ",
                "/d",
                &cmd,
                "/f",
            ])
            .status();
        hbb_common::log::info!("[EDGE] 已注册开机自启 (HKCU Run)");
    }
}

#[cfg(target_os = "linux")]
fn enable_auto_start() {
    if let Ok(exe) = std::env::current_exe() {
        if let Ok(home) = std::env::var("HOME") {
            let dir = format!("{}/.config/autostart", home);
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(
                format!("{}/rustdesk-edge.desktop", dir),
                format!(
                    "[Desktop Entry]\nType=Application\nName=RustDesk Edge\nExec={}\nX-GNOME-Autostart-enabled=true\n",
                    exe.display()
                ),
            );
            hbb_common::log::info!("[EDGE] 已注册开机自启 (autostart desktop)");
        }
    }
}

#[cfg(target_os = "macos")]
fn enable_auto_start() {
    // LaunchAgent plist（按需补充）
}

#[cfg(any(target_os = "android", target_os = "ios"))]
fn enable_auto_start() {
    // 移动端受系统后台策略限制（Android BootReceiver / iOS 前台受限），由平台策略决定
}
