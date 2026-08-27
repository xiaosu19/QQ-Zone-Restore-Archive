use std::{
    collections::HashMap,
    sync::atomic::{AtomicUsize, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use regex::Regex;
use reqwest::{
    header::{COOKIE, USER_AGENT},
    redirect::Policy,
    Client, Response,
};
use serde::Serialize;
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
use tokio::sync::Mutex;
use url::Url;

const APP_ID: &str = "549000929";
const DAID: &str = "5";
const MOBILE_USER_AGENTS: &[&str] = &[
    "Mozilla/5.0 (iPhone; CPU iPhone OS 18_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.5 Mobile/15E148 Safari/604.1",
    "Mozilla/5.0 (iPhone; CPU iPhone OS 17_6 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.6 Mobile/15E148 Safari/604.1",
    "Mozilla/5.0 (Linux; Android 15; Pixel 8 Build/AP3A.241105.007) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Mobile Safari/537.36",
    "Mozilla/5.0 (Linux; Android 14; SM-S9280 Build/UP1A.231005.007) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Mobile Safari/537.36",
    "Mozilla/5.0 (Linux; Android 14; 23127PN0CC Build/UKQ1.231003.002) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Mobile Safari/537.36",
    "Mozilla/5.0 (Linux; Android 14; V2309A Build/UP1A.231005.007) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Mobile Safari/537.36",
];
static USER_AGENT_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

const WEB_LOGIN_URL: &str = "https://i.qq.com";
const WEB_LOGIN_WINDOW_LABEL: &str = "qq-web-login";

const XLOGIN_URL: &str = "https://xui.ptlogin2.qq.com/cgi-bin/xlogin";
const S_URL: &str = "https://h5.qzone.qq.com/mqzone/index";
const PROXY_URL: &str = "";

#[derive(Default)]
struct LoginSession {
    ptqrtoken: i64,
    cookies: HashMap<String, String>,
    uin: Option<String>,
    g_tk: Option<i64>,
    user_agent: String,
    login_sig: String,
}

pub struct QLoginState {
    client: Client,
    session: Mutex<Option<LoginSession>>,
    last_user_agent: Mutex<Option<String>>,
}

pub(crate) struct QzoneAuth {
    pub uin: String,
    pub g_tk: i64,
    pub cookie_header: String,
    pub desktop_cookie_header: String,
    pub user_agent: String,
}

impl QLoginState {
    pub fn new() -> Self {
        let client = Client::builder()
            .redirect(Policy::none())
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(35))
            .build()
            .expect("failed to build QQ login HTTP client");
        Self {
            client,
            session: Mutex::new(None),
            last_user_agent: Mutex::new(None),
        }
    }

    pub(crate) fn client(&self) -> Client {
        self.client.clone()
    }

    pub(crate) async fn qzone_auth(&self) -> Result<QzoneAuth, String> {
        let guard = self.session.lock().await;
        let session = guard.as_ref().ok_or("尚未登录 QQ 空间")?;
        let g_tk = session.g_tk.ok_or("登录会话缺少 g_tk")?;
        if session
            .cookies
            .get("p_skey")
            .is_none_or(|value| value.trim().is_empty())
        {
            return Err("登录会话缺少有效的 p_skey".into());
        }
        let uin = session.uin.clone().ok_or("登录会话缺少 uin")?;
        Ok(QzoneAuth {
            uin,
            g_tk,
            cookie_header: cookie_header(&session.cookies),
            desktop_cookie_header: desktop_cookie_header(&session.cookies),
            user_agent: session.user_agent.clone(),
        })
    }

    async fn next_mobile_user_agent(&self) -> String {
        let mut previous = self.last_user_agent.lock().await;
        let selected = select_mobile_user_agent(previous.as_deref());
        *previous = Some(selected.clone());
        selected
    }

    pub(crate) async fn clear_session(&self) {
        *self.session.lock().await = None;
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QrLoginStart {
    qr_image: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginCredentials {
    uin: String,
    g_tk: i64,
    cookies: HashMap<String, String>,
    user_agent: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginStatus {
    status: &'static str,
    message: String,
    auth: Option<LoginCredentials>,
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn random_hex(len: usize) -> String {
    let seed = unix_millis() ^ (USER_AGENT_SEQUENCE.load(Ordering::Relaxed) as u128);
    let mut state = seed.wrapping_mul(0x9E37_79B9);
    let mut result = String::with_capacity(len);
    for _ in 0..len {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let digit = ((state >> 32) & 0xF) as u8;
        result.push(char::from_digit(digit as u32, 16).unwrap_or('0'));
    }
    result
}

/// 生成随机大小写字母+数字混合串，用于需要全字符集的 Cookie（如 RK）。
fn random_alphanum(len: usize) -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let seed = unix_millis() ^ (USER_AGENT_SEQUENCE.load(Ordering::Relaxed) as u128);
    let mut state = seed.wrapping_mul(0x9E37_79B9);
    let mut result = String::with_capacity(len);
    for _ in 0..len {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let idx = ((state >> 32) as usize) % CHARSET.len();
        result.push(CHARSET[idx] as char);
    }
    result
}

fn select_mobile_user_agent(previous: Option<&str>) -> String {
    let sequence = USER_AGENT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let seed = unix_millis() as usize ^ sequence.wrapping_mul(0x9E37_79B1);
    let mut index = seed % MOBILE_USER_AGENTS.len();
    if previous.is_some_and(|value| value == MOBILE_USER_AGENTS[index]) {
        index = (index + 1) % MOBILE_USER_AGENTS.len();
    }
    MOBILE_USER_AGENTS[index].to_owned()
}

fn account_user_agent(uin: &str) -> String {
    let hash = uin
        .bytes()
        .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    MOBILE_USER_AGENTS[hash as usize % MOBILE_USER_AGENTS.len()].to_owned()
}

fn callback_query_value(text: &str, name: &str) -> Option<String> {
    let pattern = format!(r"(?:[?&]|'){name}=([^&']+)");
    Regex::new(&pattern)
        .ok()?
        .captures(text)
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str().to_owned())
}

fn ptqr_token(qrsig: &str) -> i64 {
    let mut value: u32 = 0;
    for character in qrsig.chars() {
        value = value
            .wrapping_add(value.wrapping_shl(5))
            .wrapping_add(character as u32);
    }
    (value & 0x7fff_ffff) as i64
}

fn bkn(p_skey: &str) -> i64 {
    let mut value: u32 = 5381;
    for character in p_skey.chars() {
        value = value
            .wrapping_add(value.wrapping_shl(5))
            .wrapping_add(character as u32);
    }
    (value & 0x7fff_ffff) as i64
}

#[cfg(test)]
mod tests {
    use super::{
        bkn, callback_query_value, ptqr_token, select_mobile_user_agent, MOBILE_USER_AGENTS,
    };

    #[test]
    fn login_hashes_match_reference_algorithm() {
        assert_eq!(ptqr_token("abc"), 108_966);
        assert_eq!(bkn("abc"), 193_485_963);
    }

    #[test]
    fn login_hashes_wrap_without_panicking() {
        let long_value = "qrsig".repeat(1_000);
        assert!((0..=0x7fff_ffff).contains(&ptqr_token(&long_value)));
        assert!((0..=0x7fff_ffff).contains(&bkn(&long_value)));
    }

    #[test]
    fn extracts_login_values_from_callback_url() {
        let response = "ptuiCB('0','0','https://ptlogin2.qzone.qq.com/check_sig?uin=o01941163264&ptsigx=abc123&service=ptqrlogin','0','登录成功！','昵称');";
        assert_eq!(
            callback_query_value(response, "uin").as_deref(),
            Some("o01941163264")
        );
        assert_eq!(
            callback_query_value(response, "ptsigx").as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn selects_real_mobile_user_agents_and_avoids_previous_one() {
        for user_agent in MOBILE_USER_AGENTS {
            assert!(user_agent.starts_with("Mozilla/5.0"));
            assert!(user_agent.contains("iPhone") || user_agent.contains("Android"));
            assert!(user_agent.contains("Mobile"));
        }
        let previous = MOBILE_USER_AGENTS[0];
        let selected = select_mobile_user_agent(Some(previous));
        assert!(MOBILE_USER_AGENTS.contains(&selected.as_str()));
        assert_ne!(selected, previous);
    }
}

fn cookie_header(cookies: &HashMap<String, String>) -> String {
    cookies
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("; ")
}

// The legacy desktop Qzone feeds APIs are unusually sensitive to the exact
// authentication cookie set. Keep this in the same order and shape used by a
// normal desktop request (and by GetQzonehistory) instead of forwarding the
// mobile fingerprint cookies collected during QR login.
fn desktop_cookie_header(cookies: &HashMap<String, String>) -> String {
    let p_uin = cookies
        .get("p_uin")
        .or_else(|| cookies.get("uin"))
        .map(String::as_str)
        .unwrap_or_default();
    [
        ("uin", p_uin),
        (
            "skey",
            cookies.get("skey").map(String::as_str).unwrap_or_default(),
        ),
        ("p_uin", p_uin),
        (
            "pt4_token",
            cookies
                .get("pt4_token")
                .map(String::as_str)
                .unwrap_or_default(),
        ),
        (
            "p_skey",
            cookies
                .get("p_skey")
                .map(String::as_str)
                .unwrap_or_default(),
        ),
    ]
    .into_iter()
    .filter(|(_, value)| !value.is_empty())
    .map(|(name, value)| format!("{name}={value}"))
    .collect::<Vec<_>>()
    .join(";")
}

fn merge_response_cookies(response: &Response, cookies: &mut HashMap<String, String>) {
    for cookie in response.cookies() {
        let value = cookie.value().trim();
        // QQ 的响应可能同时带有清理旧 Cookie 的空值，不能让它覆盖本次登录得到的有效值。
        if !value.is_empty() {
            cookies.insert(cookie.name().to_owned(), value.to_owned());
        }
    }
}

fn normalized_uin(value: &str) -> String {
    value
        .trim_start_matches('o')
        .trim_start_matches('0')
        .to_owned()
}

async fn fetch_login_sig(
    client: &Client,
    user_agent: &str,
) -> Result<(String, HashMap<String, String>), String> {
    let params = [
        ("hide_title_bar", "1"),
        ("style", "22"),
        ("daid", DAID),
        ("low_login", "0"),
        ("qlogin_auto_login", "1"),
        ("no_verifyimg", "1"),
        ("link_target", "blank"),
        ("appid", APP_ID),
        ("target", "self"),
        ("s_url", S_URL),
        ("proxy_url", PROXY_URL),
        ("pt_no_auth", "1"),
    ];
    let response = client
        .get(XLOGIN_URL)
        .header(USER_AGENT, user_agent)
        .query(&params)
        .send()
        .await
        .map_err(|e| format!("xlogin 请求失败: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("xlogin 返回 HTTP {}", response.status()));
    }
    let mut cookies = HashMap::new();
    merge_response_cookies(&response, &mut cookies);
    let sig = cookies
        .remove("pt_login_sig")
        .ok_or("xlogin 响应中缺少 pt_login_sig cookie")?;
    Ok((sig, cookies))
}

fn poll_login_url(text: &str) -> Option<String> {
    let re = Regex::new(r"'([^']*)'").ok()?;
    let values: Vec<&str> = re
        .captures_iter(text)
        .filter_map(|c| c.get(1))
        .map(|m| m.as_str())
        .collect();
    (values.len() >= 3 && values[0] == "0").then(|| values[2].to_owned())
}

fn login_credentials(session: &LoginSession) -> Option<LoginCredentials> {
    let uin = session.uin.clone()?;
    let g_tk = session.g_tk?;
    let allowed = ["uin", "skey", "p_uin", "pt4_token", "p_skey", "pt2gguin"];
    let cookies = session
        .cookies
        .iter()
        .filter(|(name, value)| allowed.contains(&name.as_str()) && !value.is_empty())
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    Some(LoginCredentials {
        uin,
        g_tk,
        cookies,
        user_agent: session.user_agent.clone(),
    })
}

/// 登录成功后访问 QQ 空间 H5 首页，收集追踪 Cookie 并设置用户标识。
async fn warmup_qzone_session(
    client: &Client,
    cookies: &mut HashMap<String, String>,
    user_agent: &str,
    uin: &str,
) {
    // 访问 H5 QQ 空间首页，触发服务端设置完整的追踪 Cookie
    if let Ok(response) = client
        .get("https://h5.qzone.qq.com/mqzone/index")
        .header(USER_AGENT, user_agent)
        .header(COOKIE, cookie_header(cookies))
        .send()
        .await
    {
        if response.status().is_success() || response.status().is_redirection() {
            merge_response_cookies(&response, cookies);
        }
    }
    // 用户标识 Cookie（正常浏览器由客户端 JS 设置）
    if !cookies.contains_key("ptui_loginuin") {
        cookies.insert("ptui_loginuin".into(), uin.to_owned());
    }
    cookies
        .entry("QZ_FE_WEBP_SUPPORT".to_owned())
        .or_insert_with(|| "1".into());
    cookies
        .entry("cpu_performance_v8".to_owned())
        .or_insert_with(|| "0".into());
    cookies
        .entry("__Q_w_s_hat_seed".to_owned())
        .or_insert_with(|| "1".into());
    cookies
        .entry("domainid".to_owned())
        .or_insert_with(|| "5".into());
}

#[tauri::command]
pub async fn start_qr_login(state: tauri::State<'_, QLoginState>) -> Result<QrLoginStart, String> {
    let user_agent = state.next_mobile_user_agent().await;
    let (login_sig, mut cookies) = fetch_login_sig(&state.client, &user_agent).await?;
    let response = state
        .client
        .get("https://ssl.ptlogin2.qq.com/ptqrshow")
        .header(USER_AGENT, &user_agent)
        .header(COOKIE, cookie_header(&cookies))
        .query(&[
            ("appid", APP_ID),
            ("e", "2"),
            ("l", "M"),
            ("s", "3"),
            ("d", "72"),
            ("v", "4"),
            ("t", &unix_millis().to_string()),
            ("daid", DAID),
            ("pt_3rd_aid", "0"),
            ("u1", S_URL),
        ])
        .send()
        .await
        .map_err(|error| format!("获取登录二维码失败：{error}"))?;
    if !response.status().is_success() {
        return Err(format!("获取登录二维码失败：HTTP {}", response.status()));
    }
    let qrsig = response
        .cookies()
        .find(|cookie| cookie.name() == "qrsig")
        .map(|cookie| cookie.value().to_owned())
        .ok_or("二维码响应中缺少 qrsig")?;
    merge_response_cookies(&response, &mut cookies);
    // 移动端指纹 Cookie（模拟手机 QQ 浏览器）
    cookies.insert("_qimei_fingerprint".into(), random_hex(32));
    cookies.insert("_qimei_uuid42".into(), random_hex(42));
    cookies.insert("_qimei_i_3".into(), random_hex(87));
    cookies.insert(
        "_qimei_h38".into(),
        format!("{}0{}", random_hex(25), random_hex(12)),
    );
    cookies.insert("_qimei_i_1".into(), random_hex(97));
    cookies.insert(
        "_qpsvr_localtk".into(),
        format!("{:.16}", unix_millis() as f64 / 1e18),
    );
    // 浏览器追踪 Cookie（优先使用服务端返回值，仅作 fallback）
    cookies
        .entry("RK".to_owned())
        .or_insert_with(|| random_alphanum(10));
    cookies
        .entry("ptcz".to_owned())
        .or_insert_with(|| random_hex(64));
    let ts = unix_millis();
    cookies
        .entry("pgv_pvid".to_owned())
        .or_insert_with(|| format!("{}", ts % 9_000_000_000 + 1_000_000_000));
    cookies
        .entry("pgv_info".to_owned())
        .or_insert_with(|| format!("ssid=s{}", ts));
    cookies
        .entry("QZ_FE_WEBP_SUPPORT".to_owned())
        .or_insert_with(|| "1".into());
    cookies
        .entry("cpu_performance_v8".to_owned())
        .or_insert_with(|| "0".into());
    cookies
        .entry("__Q_w_s_hat_seed".to_owned())
        .or_insert_with(|| "1".into());
    cookies
        .entry("domainid".to_owned())
        .or_insert_with(|| "5".into());
    cookies.entry("fqm_pvqid".to_owned()).or_insert_with(|| {
        format!(
            "{}-{}-{}-{}-{}",
            random_hex(8),
            random_hex(4),
            random_hex(4),
            random_hex(4),
            random_hex(12)
        )
    });
    cookies
        .entry("fqm_sessionid".to_owned())
        .or_insert_with(|| {
            format!(
                "{}-{}-{}-{}-{}",
                random_hex(8),
                random_hex(4),
                random_hex(4),
                random_hex(4),
                random_hex(12)
            )
        });
    let image = response
        .bytes()
        .await
        .map_err(|error| format!("读取二维码失败：{error}"))?;
    *state.session.lock().await = Some(LoginSession {
        ptqrtoken: ptqr_token(&qrsig),
        cookies,
        user_agent,
        login_sig,
        ..Default::default()
    });
    Ok(QrLoginStart {
        qr_image: format!("data:image/png;base64,{}", BASE64.encode(image)),
    })
}

#[tauri::command]
pub async fn poll_qr_login(state: tauri::State<'_, QLoginState>) -> Result<LoginStatus, String> {
    let mut guard = state.session.lock().await;
    let session = guard.as_mut().ok_or("请先获取登录二维码")?;
    let response = state
        .client
        .get("https://ssl.ptlogin2.qq.com/ptqrlogin")
        .header(USER_AGENT, &session.user_agent)
        .header(COOKIE, cookie_header(&session.cookies))
        .query(&[
            ("u1", S_URL),
            ("ptqrtoken", &session.ptqrtoken.to_string()),
            ("ptredirect", "0"),
            ("h", "1"),
            ("t", "1"),
            ("g", "1"),
            ("from_ui", "1"),
            ("ptlang", "2052"),
            ("action", &format!("0-0-{}", unix_millis())),
            ("js_ver", "20032614"),
            ("js_type", "1"),
            ("login_sig", &session.login_sig),
            ("pt_uistyle", "40"),
            ("has_onekey", "1"),
            ("o1vId", ""),
            ("aid", APP_ID),
            ("daid", DAID),
        ])
        .send()
        .await
        .map_err(|error| format!("检查扫码状态失败：{error}"))?;
    merge_response_cookies(&response, &mut session.cookies);
    let text = response
        .text()
        .await
        .map_err(|error| format!("读取扫码状态失败：{error}"))?;

    if text.contains("'66'") || text.contains("二维码未失效") {
        return Ok(LoginStatus {
            status: "waiting",
            message: "请使用手机 QQ 扫描二维码".into(),
            auth: None,
        });
    }
    if text.contains("'67'") || text.contains("二维码认证中") {
        return Ok(LoginStatus {
            status: "scanned",
            message: "已扫码，请在手机上确认登录".into(),
            auth: None,
        });
    }
    if text.contains("'65'") || text.contains("二维码已失效") {
        return Ok(LoginStatus {
            status: "expired",
            message: "二维码已失效，请刷新后重试".into(),
            auth: None,
        });
    }
    if !(text.contains("'0'") || text.contains("登录成功")) {
        return Ok(LoginStatus {
            status: "error",
            message: "QQ 登录返回了无法识别的状态".into(),
            auth: None,
        });
    }

    let login_url = poll_login_url(&text).unwrap_or_else(|| {
        let ptsigx = callback_query_value(&text, "ptsigx").unwrap_or_default();
        let uin = callback_query_value(&text, "uin").unwrap_or_default();
        format!("https://ptlogin2.qzone.qq.com/check_sig?pttype=1&uin={uin}&service=ptqrlogin&nodirect=0&ptsigx={ptsigx}&s_url={S_URL}&f_url=&ptlang=2052&ptredirect=100&aid={APP_ID}&daid={DAID}")
    });
    let callback_uin = callback_query_value(&text, "uin").ok_or("登录成功响应中缺少 uin")?;
    let response = state
        .client
        .get(&login_url)
        .header(USER_AGENT, &session.user_agent)
        .header(COOKIE, cookie_header(&session.cookies))
        .send()
        .await
        .map_err(|error| format!("确认 QQ 登录失败：{error}"))?;
    merge_response_cookies(&response, &mut session.cookies);
    let uin = normalized_uin(&callback_uin);
    let p_skey = session
        .cookies
        .get("p_skey")
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            let available = session
                .cookies
                .iter()
                .filter(|(_, value)| !value.trim().is_empty())
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            format!("登录 Cookie 中缺少有效的 p_skey（当前 Cookie：{available}）")
        })?;
    session.g_tk = Some(bkn(p_skey));
    session.uin = Some(uin.clone());
    session.user_agent = account_user_agent(&uin);
    // 预热：访问 H5 QQ 空间首页，收集完整的追踪 Cookie
    let warmup_ua = session.user_agent.clone();
    warmup_qzone_session(&state.client, &mut session.cookies, &warmup_ua, &uin).await;
    let auth = login_credentials(session).ok_or("登录凭证不完整")?;
    Ok(LoginStatus {
        status: "success",
        message: "登录成功".into(),
        auth: Some(auth),
    })
}

#[tauri::command]
pub async fn get_login_status(state: tauri::State<'_, QLoginState>) -> Result<LoginStatus, String> {
    let guard = state.session.lock().await;
    if let Some(session) = guard.as_ref() {
        if let Some(auth) = login_credentials(session) {
            return Ok(LoginStatus {
                status: "success",
                message: "已登录".into(),
                auth: Some(auth),
            });
        }
    }
    Ok(LoginStatus {
        status: "loggedOut",
        message: "尚未登录".into(),
        auth: None,
    })
}

#[tauri::command]
pub async fn logout_qzone(state: tauri::State<'_, QLoginState>) -> Result<(), String> {
    state.clear_session().await;
    Ok(())
}

#[tauri::command]
pub async fn open_web_login(app: tauri::AppHandle) -> Result<LoginStatus, String> {
    if let Some(window) = app.get_webview_window(WEB_LOGIN_WINDOW_LABEL) {
        window.set_focus().ok();
        return Ok(LoginStatus {
            status: "webLoginOpened",
            message: "登录窗口已打开，请在窗口中完成 QQ 登录".into(),
            auth: None,
        });
    }

    let builder = WebviewWindowBuilder::new(
        &app,
        WEB_LOGIN_WINDOW_LABEL,
        WebviewUrl::External(
            WEB_LOGIN_URL
                .parse::<Url>()
                .map_err(|e| format!("登录地址无效: {e}"))?,
        ),
    )
    .title("QQ 账号登录")
    .inner_size(800.0, 720.0);
    #[cfg(desktop)]
    let builder = builder.center();
    builder
        .build()
        .map_err(|e| format!("创建登录窗口失败: {e}"))?;

    Ok(LoginStatus {
        status: "webLoginOpened",
        message: "请在打开的窗口中完成 QQ 登录".into(),
        auth: None,
    })
}

#[tauri::command]
pub async fn check_web_login(
    app: tauri::AppHandle,
    state: tauri::State<'_, QLoginState>,
) -> Result<LoginStatus, String> {
    let Some(window) = app.get_webview_window(WEB_LOGIN_WINDOW_LABEL) else {
        return Ok(LoginStatus {
            status: "webLoginCancelled",
            message: "登录窗口已关闭".into(),
            auth: None,
        });
    };

    let url = Url::parse("https://i.qq.com").map_err(|e| format!("{e}"))?;

    let (cookies, all_cookies) = tokio::task::spawn_blocking(move || {
        let cookies = window.cookies_for_url(url).unwrap_or_default();
        let all = window.cookies().unwrap_or_default();
        (cookies, all)
    })
    .await
    .map_err(|e| format!("读取 Cookie 线程异常: {e}"))?;

    let mut cookie_map: HashMap<String, String> = HashMap::new();
    for c in &cookies {
        cookie_map.insert(c.name().to_string(), c.value().to_string());
    }
    // Fallback: merge all_cookies if url-scoped didn't get p_skey
    if !cookie_map.get("p_skey").is_some_and(|v| !v.is_empty()) {
        for c in &all_cookies {
            let name = c.name().to_string();
            if !cookie_map.contains_key(&name) {
                cookie_map.insert(name, c.value().to_string());
            }
        }
    }

    let p_skey = match cookie_map.get("p_skey").filter(|v| !v.is_empty()) {
        Some(v) => v.clone(),
        None => {
            return Ok(LoginStatus {
                status: "webLoginWaiting",
                message: "等待登录完成…".into(),
                auth: None,
            });
        }
    };

    let uin = cookie_map
        .get("uin")
        .or_else(|| cookie_map.get("p_uin"))
        .filter(|v| !v.is_empty())
        .cloned()
        .ok_or_else(|| {
            let available = cookie_map.keys().cloned().collect::<Vec<_>>().join(", ");
            format!("登录 Cookie 不完整：缺少 uin（当前可用 Cookie：{available}）")
        })?;

    let g_tk = bkn(&p_skey);
    let user_agent = account_user_agent(&uin);
    let normalized = normalized_uin(&uin);

    // 预热：访问 H5 QQ 空间首页，收集完整的追踪 Cookie
    warmup_qzone_session(&state.client, &mut cookie_map, &user_agent, &normalized).await;

    let session = LoginSession {
        ptqrtoken: 0,
        cookies: cookie_map,
        uin: Some(normalized),
        g_tk: Some(g_tk),
        user_agent,
        ..Default::default()
    };

    let auth = login_credentials(&session).ok_or("登录凭证不完整")?;

    if let Some(w) = app.get_webview_window(WEB_LOGIN_WINDOW_LABEL) {
        w.close().ok();
    }

    *state.session.lock().await = Some(session);

    Ok(LoginStatus {
        status: "success",
        message: "登录成功".into(),
        auth: Some(auth),
    })
}

// Cookie 注入只服务于桌面端额外的 QQ 空间 WebView。iOS/Android 的
// WKWebView/系统 WebView 不应从异步 command 线程直接操作原生 Cookie Store，
// 否则登录成功后可能触发 WebKit 主线程/RunLoop 竞态并以 SIGABRT 退出。
#[cfg(any(target_os = "ios", target_os = "android"))]
#[tauri::command]
pub async fn sync_cookies_to_webview() -> Result<(), String> {
    Ok(())
}

#[cfg(not(any(target_os = "ios", target_os = "android")))]
#[tauri::command]
pub async fn sync_cookies_to_webview(
    app: tauri::AppHandle,
    state: tauri::State<'_, QLoginState>,
) -> Result<(), String> {
    let guard = state.session.lock().await;
    let session = guard.as_ref().ok_or("尚未登录，无法同步 Cookie")?;
    let Some(main_window) = app.get_webview_window("main") else {
        return Ok(()); // 没有主窗口则跳过
    };
    for (name, value) in &session.cookies {
        if value.trim().is_empty() {
            continue;
        }
        let cookie_str = format!("{name}={value}; Domain=.qq.com; Path=/");
        if let Ok(c) = cookie_str.parse::<cookie::Cookie>() {
            main_window.set_cookie(c).ok();
        }
    }
    Ok(())
}
