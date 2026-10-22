use crate::{
    config::{Config, MixedPort},
    core::{CoreManager, handle, tray},
    feat::clean_async,
    process::AsyncHandler,
    utils,
};
use bytes::BytesMut;
use clash_verge_logging::{Type, logging};
use once_cell::sync::Lazy;
use serde_yaml_ng::{Mapping, Value};
use smartstring::alias::String;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

#[allow(clippy::expect_used)]
static TLS_CONFIG: Lazy<Arc<rustls::ClientConfig>> = Lazy::new(|| {
    let root_store = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("Failed to set TLS versions")
        .with_root_certificates(root_store)
        .with_no_client_auth();
    Arc::new(config)
});

/// Restart the Clash core
pub async fn restart_clash_core() {
    match CoreManager::global().restart_core().await {
        Ok(_) => {
            handle::Handle::refresh_clash();
            handle::Handle::notice_message("set_config::ok", "ok");
        }
        Err(err) => {
            handle::Handle::notice_message("set_config::error", format!("{err}"));
            logging!(error, Type::Core, "{err}");
        }
    }
}

/// Restart the application
pub async fn restart_app() {
    logging!(debug, Type::System, "启动重启应用流程");
    // 设置退出标志
    handle::Handle::global().set_is_exiting();

    Config::apply_all_and_save_file().await;

    logging!(info, Type::System, "开始异步清理资源");
    let cleanup_result = clean_async().await;

    logging!(
        info,
        Type::System,
        "资源清理完成，退出代码: {}",
        if cleanup_result.all_success { 0 } else { 1 }
    );

    if !cleanup_result.core_stopped {
        handle::Handle::global().clear_is_exiting();
        handle::Handle::notice_message("app_restart::core_stop_failed", "");
        return;
    }

    utils::server::shutdown_embedded_server();
    let app_handle = handle::Handle::app_handle();
    app_handle.restart();
}

fn after_change_clash_mode() {
    AsyncHandler::spawn(move || async {
        let mihomo = handle::Handle::mihomo();
        match mihomo.get_connections().await {
            Ok(connections) => {
                if let Some(connections_array) = connections.connections {
                    for connection in connections_array {
                        let _ = mihomo.close_connection(&connection.id).await;
                    }
                }
            }
            Err(err) => {
                logging!(error, Type::Core, "Failed to get connections: {err}");
            }
        }
    });
}

/// Change Clash mode (rule/global/direct/script)
///
/// mihomo `/configs` PATCH 失败时返回 `Err`，以便命令层把失败上抛给前端。
/// （此前该函数吞掉错误并始终视为成功，导致 UI 误判"切换成功"、看似"切不动"。）
pub async fn change_clash_mode(mode: String) -> Result<(), String> {
    let mut mapping = Mapping::new();
    mapping.insert(Value::from("mode"), Value::from(mode.as_str()));
    // Convert YAML mapping to JSON Value
    let json_value = serde_json::json!({
        "mode": mode
    });
    logging!(debug, Type::Core, "change clash mode to {mode}");
    if let Err(err) = handle::Handle::mihomo().patch_base_config(&json_value).await {
        logging!(error, Type::Core, "{err}");
        return Err(err.to_string().into());
    }

    // 更新订阅
    let clash = Config::clash().await;
    clash.edit_draft(|d| d.patch_config(&mapping));
    clash.apply();

    // 分离数据获取和异步调用
    let clash_data = clash.data_arc();
    if clash_data.save_config().await.is_ok() {
        handle::Handle::refresh_clash();
        tray::Tray::global().update_menu_and_icon().await;
    }

    let is_auto_close_connection = Config::verge().await.data_arc().auto_close_connection.unwrap_or(false);
    if is_auto_close_connection {
        after_change_clash_mode();
    }

    Ok(())
}

/// Refresh delay history for the main proxy group through mihomo's group delay API.
///
/// Invoked by the timer after each scheduled profile update so the latency
/// colors stay current without a manual click. The frontend falls back to the
/// freshest mihomo delay-history entry when it has no fresh measurement of its
/// own, so refreshing the history (then emitting refresh_clash) is enough.
///
/// Returns true when the (single) tested group measured successfully; the
/// caller should skip auto-selection for this round otherwise, so stale
/// history never feeds a switch decision.
pub async fn refresh_all_group_delays() -> bool {
    use tauri_plugin_mihomo::models::ProxyType;

    /// 界面只展示主分组，自动选举也只读主分组，测速范围收敛到它，
    /// 避免每轮对全部分组数百个节点发探测把代理通道打满、误伤当前节点。
    const MAIN_GROUP: &str = "节点选择";

    let verge = Config::verge().await.data_arc();
    let test_url = verge
        .default_latency_test
        .clone()
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| "http://cp.cloudflare.com/generate_204".into());
    let timeout_ms = verge.default_latency_timeout.unwrap_or(5000).clamp(1000, 30000) as u32;
    drop(verge);

    let mihomo = handle::Handle::mihomo();
    let proxies = match mihomo.get_proxies().await {
        Ok(proxies) => proxies,
        Err(err) => {
            logging!(error, Type::Timer, "自动延迟测试：获取代理列表失败: {err}");
            return false;
        }
    };

    let mut groups: Vec<std::string::String> = proxies
        .proxies
        .values()
        .filter(|proxy| {
            // 只测 Selector：对 URLTest/Fallback 发组测速会清掉用户手点的 fixed，
            // 而这些自动组本身有自测间隔，不需要我们代测
            matches!(proxy.proxy_type, ProxyType::Selector) && !proxy.hidden.unwrap_or(false)
        })
        .map(|proxy| proxy.name.clone())
        .collect();

    if groups.is_empty() {
        logging!(debug, Type::Timer, "自动延迟测试：没有可测试的代理分组");
        return false;
    }

    // 只测主分组：前端只展示它、选举只读它，全量测速纯属负载
    if let Some(pos) = groups.iter().position(|name| name == MAIN_GROUP) {
        groups = vec![groups.swap_remove(pos)];
    } else {
        groups.truncate(1);
    }

    logging!(info, Type::Timer, "开始自动延迟测试，分组数: {}", groups.len());

    // 逐个串行测试：mihomo 的 named-pipe IPC 无法承受多分组并发请求，
    // 并发会让大部分分组直接 "error sending request"。
    let mut tested_nodes = 0usize;
    let mut all_ok = true;
    for group in &groups {
        match mihomo.delay_group(group, &test_url, timeout_ms).await {
            Ok(delays) => {
                tested_nodes += delays.len();
                logging!(
                    debug,
                    Type::Timer,
                    "分组 {} 延迟测试完成，节点数: {}",
                    group,
                    delays.len()
                );
            }
            Err(err) => {
                all_ok = false;
                logging!(error, Type::Timer, "分组 {} 延迟测试失败: {err}", group);
            }
        }
    }

    logging!(info, Type::Timer, "自动延迟测试完成，测试节点总数: {}", tested_nodes);
    handle::Handle::refresh_clash();
    all_ok
}

/// 每次刷新后自动把「节点选择」锁定到延迟最低的 Hysteria 节点。
///
/// 2026-10-21 用户改判：不再局限于 🇸🇬 SG新加坡 / 🇯🇵 JP日本 系列，
/// 任何地区系列均可，只按 Hysteria/Hysteria2 协议过滤（AnyTLS 等其他协议实测慢，排除），
/// 在测活节点中选延迟最低者。
///
/// 防抖设计（2026-10-22，修 4 小时抖动 7 次的回归）：
/// 每次 select_node_for_group 都会掐断在用 TCP 连接，切换必须保守：
/// - 当前节点单轮测速超时（delay=0 或缺 history）不直接判死，先单独复测；
///   连续 DEAD_AFTER_TIMEOUT_ROUNDS 轮复测都失败才认定节点死亡（故障切换，不受驻留限制）。
/// - 纯延迟优势切换要求最优比当前快超过 HYSTERESIS_MS，且距上次自动切换满 MIN_DWELL。
static LAST_AUTO_SWITCH: parking_lot::Mutex<Option<Instant>> = parking_lot::Mutex::new(None);
static CURRENT_TIMEOUT_ROUNDS: AtomicU32 = AtomicU32::new(0);

pub async fn auto_select_best_hysteria_node() {
    use tauri_plugin_mihomo::models::ProxyType;

    const MAIN_GROUP: &str = "节点选择";
    const HYSTERESIS_MS: u16 = 150;
    const DEAD_AFTER_TIMEOUT_ROUNDS: u32 = 2;
    const MIN_DWELL: Duration = Duration::from_secs(30 * 60);

    // 自动选节点总开关关闭（用户手动模式）：本次刷新不做自动切换，
    // 保持用户当前所选；ON 模式按「全自动」取延迟最优，不做手选钉住。
    let verge = Config::verge().await.latest_arc();
    if verge.enable_auto_select == Some(false) {
        logging!(
            info,
            Type::Timer,
            "自动选节点：开关已关闭（手动模式），本次跳过自动切换"
        );
        return;
    }
    let test_url = verge
        .default_latency_test
        .clone()
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| "http://cp.cloudflare.com/generate_204".into());
    let timeout_ms = verge.default_latency_timeout.unwrap_or(5000).clamp(1000, 30000) as u32;
    drop(verge);

    let mihomo = handle::Handle::mihomo();
    let proxies = match mihomo.get_proxies().await {
        Ok(proxies) => proxies,
        Err(err) => {
            logging!(error, Type::Timer, "自动选节点：获取代理列表失败: {err}");
            return;
        }
    };

    // 只按协议过滤：AnyTLS 等其他协议实测慢，排除；地区不限
    let is_target_node = |name: &str| {
        proxies
            .proxies
            .get(name)
            .is_some_and(|p| matches!(p.proxy_type, ProxyType::Hysteria | ProxyType::Hysteria2))
    };

    // 主选择分组：优先「节点选择」，兜底取第一个成员含 Hysteria 节点的 Selector
    let group = proxies
        .proxies
        .get(MAIN_GROUP)
        .filter(|proxy| proxy.proxy_type == ProxyType::Selector)
        .or_else(|| {
            proxies.proxies.values().find(|proxy| {
                proxy.proxy_type == ProxyType::Selector
                    && proxy
                        .all
                        .as_ref()
                        .is_some_and(|all| all.iter().any(|name| is_target_node(name)))
            })
        });

    let Some(group) = group else {
        logging!(warn, Type::Timer, "自动选节点：找不到主选择分组，跳过");
        return;
    };

    let latest_delay = |name: &str| -> Option<u16> {
        proxies
            .proxies
            .get(name)
            .and_then(|p| p.history.last())
            .map(|h| h.delay)
    };

    let Some(all) = group.all.as_ref() else {
        logging!(warn, Type::Timer, "自动选节点：分组 {} 没有成员列表", group.name);
        return;
    };

    // history 里 delay=0 表示超时，只从测活成功的目标节点里挑最优
    let best = all
        .iter()
        .filter(|name| is_target_node(name))
        .filter_map(|name| {
            latest_delay(name)
                .filter(|delay| *delay > 0)
                .map(|delay| (name.clone(), delay))
        })
        .min_by_key(|(_, delay)| *delay);

    let Some((best_name, best_delay)) = best else {
        logging!(
            warn,
            Type::Timer,
            "自动选节点：Hysteria 节点当前全部超时或无可用节点，保持现有节点不变"
        );
        return;
    };

    let current = group.now.clone().unwrap_or_default();
    if current == best_name {
        CURRENT_TIMEOUT_ROUNDS.store(0, Ordering::Relaxed);
        logging!(
            info,
            Type::Timer,
            "自动选节点：当前已是最优节点 {} ({}ms)",
            best_name,
            best_delay
        );
        return;
    }

    let current_is_target = is_target_node(&current);
    let mut current_delay = latest_delay(&current).unwrap_or(0);
    let mut current_dead = current.is_empty() || !current_is_target;

    // 当前节点本轮测速超时（或缺 history）：单轮失败证据不足（测速负载本身会把
    // 当前节点打到超时），先单独复测；连续多轮复测都失败才判死。
    if !current_dead && current_delay == 0 {
        match mihomo.delay_proxy_by_name(&current, &test_url, timeout_ms).await {
            Ok(retest) if retest.delay > 0 => {
                current_delay = retest.delay.min(u32::from(u16::MAX)) as u16;
                CURRENT_TIMEOUT_ROUNDS.store(0, Ordering::Relaxed);
                logging!(
                    info,
                    Type::Timer,
                    "自动选节点：当前节点 {} 组测超时但单独复测恢复 ({}ms)，保持不动",
                    current,
                    current_delay
                );
            }
            _ => {
                let rounds = CURRENT_TIMEOUT_ROUNDS.fetch_add(1, Ordering::Relaxed) + 1;
                if rounds < DEAD_AFTER_TIMEOUT_ROUNDS {
                    logging!(
                        warn,
                        Type::Timer,
                        "自动选节点：当前节点 {} 第 {} 轮复测仍超时，未达判死轮数 {}，保持不动",
                        current,
                        rounds,
                        DEAD_AFTER_TIMEOUT_ROUNDS
                    );
                    return;
                }
                logging!(
                    warn,
                    Type::Timer,
                    "自动选节点：当前节点 {} 连续 {} 轮复测超时，判定死亡，允许故障切换",
                    current,
                    rounds
                );
                current_dead = true;
            }
        }
    } else if current_delay > 0 {
        CURRENT_TIMEOUT_ROUNDS.store(0, Ordering::Relaxed);
    }

    let need_switch = current_dead || best_delay.saturating_add(HYSTERESIS_MS) < current_delay;

    if !need_switch {
        logging!(
            info,
            Type::Timer,
            "自动选节点：当前节点 {} ({}ms) 与最优 {} ({}ms) 差距在阈值内，保持不动",
            current,
            current_delay,
            best_name,
            best_delay
        );
        return;
    }

    // 驻留保护：节点没死、只是别人更快时，需距上次自动切换满 MIN_DWELL 才换，
    // 避免测速波动把节点来回拨（每换一次就掐断全部在用连接）。
    if !current_dead {
        let last_switch = LAST_AUTO_SWITCH.lock();
        if let Some(last) = *last_switch {
            let elapsed = last.elapsed();
            if elapsed < MIN_DWELL {
                logging!(
                    info,
                    Type::Timer,
                    "自动选节点：最优 {} ({}ms) 快过当前 {} ({}ms)，但距上次切换 {}s 未满驻留 {}s，保持不动",
                    best_name,
                    best_delay,
                    current,
                    current_delay,
                    elapsed.as_secs(),
                    MIN_DWELL.as_secs()
                );
                return;
            }
        }
    }

    match mihomo.select_node_for_group(&group.name, &best_name).await {
        Ok(_) => {
            logging!(
                info,
                Type::Timer,
                "自动选节点：{} 切换 {} -> {} ({}ms，原节点延迟 {}ms，故障切换={})",
                group.name,
                current,
                best_name,
                best_delay,
                current_delay,
                current_dead
            );
            *LAST_AUTO_SWITCH.lock() = Some(Instant::now());
            CURRENT_TIMEOUT_ROUNDS.store(0, Ordering::Relaxed);
            handle::Handle::refresh_clash();
            tray::Tray::global().update_menu_and_icon().await;
        }
        Err(err) => logging!(error, Type::Timer, "自动选节点：切换到 {} 失败: {err}", best_name),
    }
}

/// Test delay to a URL through proxy.
/// HTTPS: measures TLS handshake time. HTTP: measures HEAD round-trip time.
pub async fn test_delay(url: String) -> anyhow::Result<u32> {
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpStream;
    use tokio::time::Instant;

    let parsed = tauri::Url::parse(&url)?;
    let is_https = parsed.scheme() == "https";
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("Invalid URL: no host"))?
        .to_string();
    let port = parsed.port().unwrap_or(if is_https { 443 } else { 80 });

    let verge = Config::verge().await.latest_arc();
    let proxy_enabled = verge.enable_system_proxy.unwrap_or(false) || verge.enable_tun_mode.unwrap_or(false);
    let proxy_port = if proxy_enabled {
        Some(MixedPort::desired().await)
    } else {
        None
    };

    tokio::time::timeout(Duration::from_secs(10), async {
        let start = Instant::now();
        let mut buf = BytesMut::with_capacity(1024);

        if is_https {
            let stream = match proxy_port {
                Some(pp) => {
                    let mut s = TcpStream::connect(format!("127.0.0.1:{pp}")).await?;
                    s.write_all(format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n\r\n").as_bytes())
                        .await?;
                    s.read_buf(&mut buf).await?;
                    if !buf.windows(3).any(|w| w == b"200") {
                        return Err(anyhow::anyhow!("Proxy CONNECT failed"));
                    }
                    s
                }
                None => TcpStream::connect(format!("{host}:{port}")).await?,
            };
            let connector = tokio_rustls::TlsConnector::from(Arc::clone(&TLS_CONFIG));
            let server_name = rustls::pki_types::ServerName::try_from(host.as_str())
                .map_err(|_| anyhow::anyhow!("Invalid DNS name: {host}"))?
                .to_owned();
            connector.connect(server_name, stream).await?;
        } else {
            let (mut stream, req) = match proxy_port {
                Some(pp) => (
                    TcpStream::connect(format!("127.0.0.1:{pp}")).await?,
                    format!("HEAD {url} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"),
                ),
                None => (
                    TcpStream::connect(format!("{host}:{port}")).await?,
                    format!("HEAD / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"),
                ),
            };
            stream.write_all(req.as_bytes()).await?;
            let _ = stream.read(&mut buf).await?;
        }

        // frontend treats 0 as timeout
        Ok((start.elapsed().as_millis() as u32).max(1))
    })
    .await
    .unwrap_or(Ok(10000u32))
}
