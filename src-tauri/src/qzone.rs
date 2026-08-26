use reqwest::header::{
    ACCEPT, ACCEPT_LANGUAGE, CACHE_CONTROL, COOKIE, ORIGIN, PRAGMA, REFERER, USER_AGENT,
};
use serde::Serialize;
use serde_json::{json, Value};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
use url::Url;
use std::sync::{Arc, Mutex};

use crate::qlogin::QLoginState;

const FEEDS_URL: &str = "https://mobile.qzone.qq.com/get_feeds";
const OWN_MOMENTS_URL: &str =
    "https://user.qzone.qq.com/proxy/domain/taotao.qq.com/cgi-bin/emotion_cgi_msglist_v6";
const HISTORY_MESSAGES_URL: &str =
    "https://user.qzone.qq.com/proxy/domain/ic2.qzone.qq.com/cgi-bin/feeds/feeds2_html_pav_all";
const DESKTOP_QZONE_USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36";
const FEED_RESPONSE_ATTEMPTS: u32 = 3;
const RECYCLE_WINDOW_LABEL: &str = "qzone-recycle-auth";
const RECYCLE_ALBUM_LIST_URL: &str =
    "https://user.qzone.qq.com/proxy/domain/photo.qzone.qq.com/cgi-bin/common/cgi_alist_recycle_v2";
const RECYCLE_PHOTO_LIST_URL: &str =
    "https://user.qzone.qq.com/proxy/domain/photo.qzone.qq.com/cgi-bin/common/cgi_plist_recycle_v2";
const RECOVER_PHOTO_URL: &str =
    "https://user.qzone.qq.com/proxy/domain/photo.qzone.qq.com/cgi-bin/common/cgi_recover_pic_v2";
const RECOVER_ALBUM_URL: &str =
    "https://user.qzone.qq.com/proxy/domain/photo.qzone.qq.com/cgi-bin/common/cgi_recover_album_v2";
const ALBUM_LIST_URL: &str =
    "https://h5.qzone.qq.com/proxy/domain/photo.qzone.qq.com/fcgi-bin/fcg_list_album_v3";
const CREATE_ALBUM_URL: &str =
    "https://user.qzone.qq.com/proxy/domain/photo.qzone.qq.com/cgi-bin/common/cgi_add_album_v2";

#[derive(Clone, Default)]
pub struct RecycleAuthState {
    pwd2sig: Arc<Mutex<Option<String>>>,
}

#[cfg(windows)]
fn pwd2sig_from_url(value: &str) -> Option<String> {
    let url = Url::parse(value).ok()?;
    url.query_pairs().find_map(|(key, value)| {
        key.eq_ignore_ascii_case("pwd2sig").then(|| value.into_owned())
    })
}

#[cfg(windows)]
fn install_recycle_request_listener(window: &tauri::WebviewWindow, state: RecycleAuthState) {
    use webview2_com::{
        Microsoft::Web::WebView2::Win32::{
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL, ICoreWebView2,
        },
        take_pwstr, WebResourceRequestedEventHandler,
    };
    use windows::core::{HSTRING, PWSTR};

    let _ = window.with_webview(move |platform| {
        let controller = platform.controller();
        let Ok(webview): Result<ICoreWebView2, _> = (unsafe { controller.CoreWebView2() }) else {
            return;
        };
        unsafe {
            let _ = webview.AddWebResourceRequestedFilter(
                &HSTRING::from("*"),
                COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
            );
            let handler = WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()); };
                let request = args.Request()?;
                let mut raw_uri = PWSTR::null();
                request.Uri(&mut raw_uri)?;
                let uri = take_pwstr(raw_uri);
                if uri.contains("cgi_plist_recycle_v2") {
                    if let Some(token) = pwd2sig_from_url(&uri) {
                        if let Ok(mut guard) = state.pwd2sig.lock() {
                            *guard = Some(token);
                        }
                    }
                }
                Ok(())
            }));
            let mut registration = std::mem::zeroed();
            let _ = webview.add_WebResourceRequested(&handler, &mut registration);
        }
    });
}

fn parse_qzone_json(text: &str) -> Result<Value, String> {
    let normalized = text.trim().trim_start_matches('\u{feff}').trim();
    if normalized.is_empty() {
        return Ok(json!({ "code": 0 }));
    }
    if let Ok(value) = serde_json::from_str(normalized) {
        return Ok(value);
    }
    if let Some(callback) = normalized.rfind("frameElement.callback(") {
        if let Some(relative_start) = normalized[callback..].find('{') {
            let start = callback + relative_start;
            if let Some(end) = normalized.rfind('}') {
                if let Ok(value) = serde_json::from_str::<Value>(&normalized[start..=end]) {
                    return Ok(value);
                }
            }
        }
    }
    // QQ may wrap JSON in `_Callback(...)`, `try{...}catch{...}` or append
    // a semicolon. Extract the outermost JSON object as a final fallback.
    // The response can be an HTML shell containing setup scripts followed by
    // a callback such as `cb({...})`. Try candidate object spans from the end
    // so setup blocks like `try { document.domain = ... }` are ignored.
    let starts: Vec<usize> = normalized.match_indices('{').map(|(index, _)| index).collect();
    let mut best_with_code: Option<(usize, Value)> = None;
    let mut fallback: Option<Value> = None;
    for &start in starts.iter().rev() {
        let ends: Vec<usize> = normalized[start..].match_indices('}').map(|(index, _)| start + index + 1).collect();
        for &end in ends.iter().rev().take(80) {
            if let Ok(value) = serde_json::from_str::<Value>(&normalized[start..end]) {
                let span = end - start;
                if value.get("code").is_some()
                    && best_with_code.as_ref().map_or(true, |(best_span, _)| span > *best_span)
                {
                    best_with_code = Some((span, value));
                } else if fallback.is_none() {
                    fallback = Some(value);
                }
            }
        }
    }
    if let Some((_, value)) = best_with_code {
        return Ok(value);
    }
    if let Some(value) = fallback {
        return Ok(value);
    }
    Err(format!("解析 QQ 空间响应失败：响应片段：{}", normalized.chars().take(180).collect::<String>()))
}

fn parse_qzone_action_response(text: &str) -> Result<Value, String> {
    if text.trim().is_empty() {
        return Ok(json!({ "code": 0 }));
    }
    parse_qzone_json(text)
}

fn ensure_qzone_success(value: Value) -> Result<Value, String> {
    let code = value.get("code").and_then(|code| {
        code.as_i64()
            .or_else(|| code.as_str().and_then(|text| text.parse().ok()))
    }).ok_or("QQ 空间响应缺少 code 字段")?;
    if code == 0 {
        return Ok(value);
    }
    let message = value
        .get("message")
        .or_else(|| value.get("msg"))
        .and_then(Value::as_str)
        .unwrap_or("未知错误");
    Err(format!("QQ 空间接口返回错误 {code}：{message}"))
}

async fn recycle_get(
    state: &QLoginState,
    url: &str,
    pwd2sig: &str,
    extra: &[(&str, String)],
) -> Result<Value, String> {
    if pwd2sig.trim().is_empty() {
        return Err("独立密码验证已失效，请重新验证".into());
    }
    let auth = state.qzone_auth().await?;
    let mut query = vec![
        ("inCharset", "utf-8".into()),
        ("outCharset", "utf-8".into()),
        ("hostUin", auth.uin.clone()),
        ("notice", "0".into()),
        ("format", "json".into()),
        ("plat", "qzone".into()),
        ("source", "qzone".into()),
        ("appid", "4".into()),
        ("uin", auth.uin.clone()),
        ("output_type", "json".into()),
        ("pwd2sig", pwd2sig.into()),
        ("g_tk", auth.g_tk.to_string()),
    ];
    query.extend(extra.iter().cloned());
    let response = state
        .client()
        .get(url)
        .header(ACCEPT, "application/json, text/javascript, */*; q=0.01")
        .header(REFERER, format!("https://user.qzone.qq.com/{}/4", auth.uin))
        .header(USER_AGENT, &auth.user_agent)
        .header(COOKIE, &auth.cookie_header)
        .query(&query)
        .send()
        .await
        .map_err(|error| format!("请求相册回收站失败：{error}"))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| format!("读取相册回收站响应失败：{error}"))?;
    if !status.is_success() {
        return Err(format!("请求相册回收站失败：HTTP {status}"));
    }
    let parsed = parse_qzone_json(&text)?;
    ensure_qzone_success(parsed)
}

#[tauri::command]
pub async fn open_recycle_password_window(
    app: tauri::AppHandle,
    state: tauri::State<'_, QLoginState>,
    recycle_state: tauri::State<'_, RecycleAuthState>,
) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(RECYCLE_WINDOW_LABEL) {
        window.set_focus().ok();
        return Ok(());
    }
    let auth = state.qzone_auth().await?;
    if let Ok(mut guard) = recycle_state.pwd2sig.lock() {
        *guard = None;
    }
    let page_url = Url::parse(&format!("https://user.qzone.qq.com/{}/photo/recycle", auth.uin))
        .map_err(|error| format!("回收站地址无效：{error}"))?;
    let bridge_script = r#"
      (() => {
        const prefix = '__QZA_PWD2SIG__';
        const publish = (token) => {
          if (typeof token !== 'string' || token.length < 5) return;
          document.title = prefix + token;
          try { history.replaceState(null, '', location.pathname + location.search + '#pwd2sig=' + encodeURIComponent(token)); } catch (_) {}
          try {
            if (window.top && window.top !== window) {
              window.top.document.title = prefix + token;
              window.top.history.replaceState(null, '', window.top.location.pathname + window.top.location.search + '#pwd2sig=' + encodeURIComponent(token));
            }
          } catch (_) {}
        };
        const capture = (input) => {
          try {
            if (input instanceof FormData || input instanceof URLSearchParams) {
              const token = input.get('pwd2sig'); if (token) publish(String(token));
              return;
            }
            const text = typeof input === 'string' ? input : input?.url || '';
            const match = text.match(/(?:^|[?&])pwd2sig=([^&]+)/i);
            if (match) publish(decodeURIComponent(match[1].replace(/\+/g, ' ')));
          } catch (_) {}
        };
        try {
          const originalOpen = XMLHttpRequest.prototype.open;
          const originalSend = XMLHttpRequest.prototype.send;
          XMLHttpRequest.prototype.open = function(method, url, ...rest) { this.__qzaUrl = String(url || ''); capture(this.__qzaUrl); return originalOpen.call(this, method, url, ...rest); };
          XMLHttpRequest.prototype.send = function(body) { capture(this.__qzaUrl); capture(body); return originalSend.call(this, body); };
        } catch (_) {}
        try {
          const originalFetch = window.fetch;
          window.fetch = function(input, init) { capture(input); capture(init?.body); return originalFetch.apply(this, arguments); };
        } catch (_) {}
        const read = (w) => {
          try {
            const dc = w.QZONE && w.QZONE.dataCenter;
            const token = dc && typeof dc.get === 'function' && dc.get('pwd2sig');
            if (typeof token === 'string' && token.length > 4) return token;
          } catch (_) {}
          try {
            const seen = new WeakSet();
            const scan = (value, depth) => {
              if (!value || depth > 4 || (typeof value !== 'object' && typeof value !== 'function')) return '';
              if (seen.has(value)) return ''; seen.add(value);
              for (const key of Object.keys(value)) {
                let child; try { child = value[key]; } catch (_) { continue; }
                if (key.toLowerCase().includes('pwd2sig') && typeof child === 'string' && child.length > 4) return child;
                const found = scan(child, depth + 1); if (found) return found;
              }
              return '';
            };
            const found = scan(w.QZONE, 0) || scan(w.QPHOTO, 0);
            if (found) return found;
            for (const storage of [w.localStorage, w.sessionStorage]) {
              for (let i = 0; i < storage.length; i++) {
                const key = storage.key(i) || ''; const value = storage.getItem(key) || '';
                if (key.toLowerCase().includes('pwd2sig') && value.length > 4) return value;
              }
            }
          } catch (_) {}
          try {
            for (let i = 0; i < w.frames.length; i++) {
              const token = read(w.frames[i]);
              if (token) return token;
            }
          } catch (_) {}
          return '';
        };
        const tick = () => {
          const token = read(window.top || window);
          if (token) publish(token);
          try {
            const roots = [document];
            for (const frame of document.querySelectorAll('iframe')) {
              if (frame.contentDocument) roots.push(frame.contentDocument);
            }
            for (const root of roots) {
              for (const node of root.querySelectorAll('*')) {
                if ((node.textContent || '').trim() === '回收站' && !sessionStorage.getItem('__qzaRecycleOpened')) {
                  sessionStorage.setItem('__qzaRecycleOpened', '1');
                  const clickable = node.closest('a,button,[role="button"]') || node;
                  clickable.click();
                  return;
                }
              }
            }
          } catch (_) {}
        };
        window.__qzaReadPwd2sig = tick;
        setInterval(tick, 800);
        setTimeout(tick, 200);
      })();
    "#;
    let builder = WebviewWindowBuilder::new(
        &app,
        RECYCLE_WINDOW_LABEL,
        WebviewUrl::External(Url::parse("about:blank").expect("about:blank 必须是有效 URL")),
    )
    .title("验证 QQ 空间独立密码")
    .inner_size(960.0, 720.0);
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    let builder = builder.center();
    let window = builder
    .initialization_script(bridge_script)
    .build()
    .map_err(|error| format!("打开独立密码验证窗口失败：{error}"))?;
    #[cfg(windows)]
    install_recycle_request_listener(&window, recycle_state.inner().clone());
    for entry in auth.cookie_header.split("; ") {
        if let Ok(cookie) = format!("{entry}; Domain=.qq.com; Path=/").parse::<cookie::Cookie>() {
            window.set_cookie(cookie).ok();
        }
    }
    window.navigate(page_url).ok();
    Ok(())
}

#[tauri::command]
pub async fn check_recycle_password(
    app: tauri::AppHandle,
    recycle_state: tauri::State<'_, RecycleAuthState>,
) -> Result<Option<String>, String> {
    if let Ok(guard) = recycle_state.pwd2sig.lock() {
        if let Some(token) = guard.clone() {
            return Ok(Some(token));
        }
    }
    let Some(window) = app.get_webview_window(RECYCLE_WINDOW_LABEL) else {
        return Ok(None);
    };
    window.eval(r#"(() => {
      const publishFromUrl = (url) => {
        try {
          const match = String(url || '').match(/(?:^|[?&])pwd2sig=([^&]+)/i);
          if (!match) return false;
          const token = decodeURIComponent(match[1].replace(/\+/g, ' '));
          history.replaceState(null, '', location.pathname + location.search + '#pwd2sig=' + encodeURIComponent(token));
          return true;
        } catch (_) { return false; }
      };
      const scanResources = (w) => {
        try {
          for (const entry of w.performance.getEntriesByType('resource')) if (publishFromUrl(entry.name)) return true;
          for (let i = 0; i < w.frames.length; i++) if (scanResources(w.frames[i])) return true;
        } catch (_) {}
        return false;
      };
      if (scanResources(window)) return;
      const seen = new WeakSet();
      const findToken = (value, depth = 0) => {
        if (!value || depth > 5 || (typeof value !== 'object' && typeof value !== 'function')) return '';
        if (seen.has(value)) return ''; seen.add(value);
        for (const key of Object.keys(value)) {
          let child; try { child = value[key]; } catch (_) { continue; }
          if (key.toLowerCase().includes('pwd2sig') && typeof child === 'string' && child.length > 4) return child;
          const found = findToken(child, depth + 1); if (found) return found;
        }
        return '';
      };
      let token = '';
      try { token = window.QZONE?.dataCenter?.get?.('pwd2sig') || ''; } catch (_) {}
      try { token = token || window.QPHOTO?.dataCenter?.get?.('pwd2sig') || ''; } catch (_) {}
      token = token || findToken(window.QZONE) || findToken(window.QPHOTO);
      try {
        for (const storage of [window.localStorage, window.sessionStorage]) {
          for (let i = 0; i < storage.length; i++) {
            const key = storage.key(i) || ''; const value = storage.getItem(key) || '';
            if (key.toLowerCase().includes('pwd2sig') && value.length > 4) token = value;
          }
        }
      } catch (_) {}
      if (token) {
        document.title = '__QZA_PWD2SIG__' + token;
        try { history.replaceState(null, '', location.pathname + location.search + '#pwd2sig=' + encodeURIComponent(token)); } catch (_) {}
      }
    })()"#).ok();
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    let title = window.title().unwrap_or_default();
    let current_url = window.url().ok().map(|url| url.to_string()).unwrap_or_default();
    let parsed_url = Url::parse(&current_url).ok();
    if let Some(token) = title.strip_prefix("__QZA_PWD2SIG__").filter(|value| !value.is_empty()) {
        return Ok(Some(token.to_owned()));
    }
    if let Ok(cookies) = window.cookies() {
        if let Some(token) = cookies
            .iter()
            .find(|cookie| cookie.name().eq_ignore_ascii_case("pwd2sig"))
            .map(|cookie| cookie.value().to_owned())
            .filter(|value| !value.is_empty())
        {
            return Ok(Some(token));
        }
    }
    // 腾讯验证成功后通常会跳转到 callback.html，并把临时签名放在查询串或 hash 中。
    let parsed = parsed_url;
    let token_from_url = parsed.as_ref().and_then(|url| {
        let from_pairs = |pairs: Vec<(String, String)>| pairs.into_iter().find_map(|(key, value)| {
            (key.eq_ignore_ascii_case("pwd2sig") || key.eq_ignore_ascii_case("pwd2Sig")).then_some(value)
        });
        from_pairs(url.query_pairs().map(|(key, value)| (key.into_owned(), value.into_owned())).collect())
            .or_else(|| from_pairs(url::form_urlencoded::parse(url.fragment().unwrap_or_default().as_bytes())
                .map(|(key, value)| (key.into_owned(), value.into_owned())).collect()))
    });
    Ok(token_from_url.filter(|value| !value.is_empty()))
}

#[tauri::command]
pub async fn close_recycle_password_window(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(RECYCLE_WINDOW_LABEL) {
        window.close().map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn list_recycle_albums(
    state: tauri::State<'_, QLoginState>,
    pwd2sig: String,
) -> Result<Value, String> {
    recycle_get(
        &state,
        RECYCLE_ALBUM_LIST_URL,
        &pwd2sig,
        &[
            ("begin", "0".into()),
            ("size", "100".into()),
            ("refresh", "true".into()),
            ("day", "0".into()),
            ("dayNum", "365".into()),
        ],
    )
    .await
}

#[tauri::command]
pub async fn list_recycle_photos(
    state: tauri::State<'_, QLoginState>,
    pwd2sig: String,
    album_id: Option<String>,
) -> Result<Value, String> {
    let mut extra = vec![
        ("begin", "0".into()),
        ("size", "18".into()),
        ("type", "0".into()),
        ("refresh", "true".into()),
        ("day", "0".into()),
        ("dayNum", "90".into()),
    ];
    if let Some(album_id) = album_id.filter(|value| !value.is_empty()) {
        extra.push(("albumId", album_id));
    }
    recycle_get(&state, RECYCLE_PHOTO_LIST_URL, &pwd2sig, &extra).await
}

#[tauri::command]
pub async fn load_recycle_photo_preview(
    state: tauri::State<'_, QLoginState>,
    image_url: String,
) -> Result<String, String> {
    let url = Url::parse(&image_url).map_err(|_| "照片缩略图地址无效".to_owned())?;
    let host = url.host_str().unwrap_or_default();
    if !(host.ends_with("qq.com") || host.ends_with("qpic.cn")) {
        return Err("照片缩略图地址不是 QQ 图片域名".into());
    }
    let auth = state.qzone_auth().await?;
    let response = state.client().get(url)
        .header(ACCEPT, "image/avif,image/webp,image/png,image/jpeg,image/*;q=0.8")
        .header(REFERER, format!("https://user.qzone.qq.com/{}/photo/recycle", auth.uin))
        .header(USER_AGENT, &auth.user_agent)
        .header(COOKIE, &auth.cookie_header)
        .send().await.map_err(|error| format!("读取照片缩略图失败：{error}"))?;
    if !response.status().is_success() { return Err(format!("读取照片缩略图失败：HTTP {}", response.status())); }
    let content_type = response.headers().get("content-type").and_then(|value| value.to_str().ok()).unwrap_or("image/jpeg").split(';').next().unwrap_or("image/jpeg").to_owned();
    let bytes = response.bytes().await.map_err(|error| format!("读取照片缩略图失败：{error}"))?;
    Ok(format!("data:{content_type};base64,{}", BASE64.encode(bytes)))
}

#[tauri::command]
pub async fn list_qzone_albums(state: tauri::State<'_, QLoginState>) -> Result<Value, String> {
    let auth = state.qzone_auth().await?;
    let request_id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .rem_euclid(1_000_000_000)
        .to_string();
    let query = vec![
        ("g_tk", auth.g_tk.to_string()),
        ("t", request_id),
        ("hostUin", auth.uin.clone()),
        ("uin", auth.uin.clone()),
        ("appid", "4".into()),
        ("inCharset", "utf-8".into()),
        ("outCharset", "utf-8".into()),
        ("source", "qzone".into()),
        ("plat", "qzone".into()),
        ("format", "jsonp".into()),
        ("notice", "0".into()),
        ("mode", "2".into()),
        ("sortOrder", "4".into()),
        ("pageStart", "0".into()),
        ("pageNum", "1000".into()),
        ("idcNum", "4".into()),
        ("callbackFun", "shine0".into()),
    ];
    let response = state
        .client()
        .get(ALBUM_LIST_URL)
        .query(&query)
        .header(ACCEPT, "application/json, text/javascript, */*; q=0.01")
        .header(REFERER, "https://user.qzone.qq.com/")
        .header(USER_AGENT, &auth.user_agent)
        .header(COOKIE, &auth.cookie_header)
        .send()
        .await
        .map_err(|error| format!("获取相册列表失败：{error}"))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| format!("读取相册列表响应失败：{error}"))?;
    if !status.is_success() {
        return Err(format!("获取相册列表失败：HTTP {status}"));
    }
    ensure_qzone_success(parse_qzone_json(&text)?)
}

#[tauri::command]
pub async fn create_qzone_album(
    state: tauri::State<'_, QLoginState>,
    name: String,
) -> Result<Value, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("相册名称不能为空".into());
    }
    if name.chars().count() > 30 {
        return Err("相册名称不能超过 30 个字符".into());
    }
    let auth = state.qzone_auth().await?;
    let form = [
        ("album_type", ""),
        ("birth_time", ""),
        ("degree_type", "0"),
        ("enroll_time", ""),
        ("albumname", name),
        ("albumdesc", ""),
        ("albumclass", "100"),
        ("priv", "1"),
        ("question", ""),
        ("answer", ""),
        ("whiteList", ""),
        ("bitmap", "10000000"),
        ("uin", auth.uin.as_str()),
        ("hostUin", auth.uin.as_str()),
        ("format", "fs"),
        ("inCharset", "utf-8"),
        ("outCharset", "utf-8"),
        ("notice", "0"),
        ("callbackFun", "_Callback"),
        ("plat", "qzone"),
        ("source", "qzone"),
        ("appid", "4"),
    ];
    let response = state
        .client()
        .post(CREATE_ALBUM_URL)
        .query(&[("g_tk", auth.g_tk.to_string())])
        .header(
            REFERER,
            format!("https://user.qzone.qq.com/{}/photo", auth.uin),
        )
        .header(USER_AGENT, &auth.user_agent)
        .header(COOKIE, &auth.cookie_header)
        .header(ORIGIN, "https://user.qzone.qq.com")
        .header(
            "content-type",
            "application/x-www-form-urlencoded;charset=UTF-8",
        )
        .form(&form)
        .send()
        .await
        .map_err(|error| format!("创建相册失败：{error}"))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| format!("读取创建相册响应失败：{error}"))?;
    if !status.is_success() {
        return Err(format!("创建相册失败：HTTP {status}"));
    }
    ensure_qzone_success(parse_qzone_action_response(&text)?)
}

#[tauri::command]
pub async fn recover_recycle_album(
    state: tauri::State<'_, QLoginState>,
    pwd2sig: String,
    album_id: String,
) -> Result<Value, String> {
    if pwd2sig.trim().is_empty() {
        return Err("独立密码验证已失效，请重新验证".into());
    }
    if album_id.trim().is_empty() {
        return Err("缺少回收站相册 ID".into());
    }
    let auth = state.qzone_auth().await?;
    let qzreferrer = format!("https://user.qzone.qq.com/{}", auth.uin);
    let form = [
        ("inCharset", "utf-8"),
        ("outCharset", "utf-8"),
        ("hostUin", auth.uin.as_str()),
        ("notice", "0"),
        ("callbackFun", "_Callback"),
        ("format", "fs"),
        ("plat", "qzone"),
        ("source", "qzone"),
        ("appid", "4"),
        ("uin", auth.uin.as_str()),
        ("albumId", album_id.as_str()),
        ("pwd2sig", pwd2sig.as_str()),
        ("qzreferrer", qzreferrer.as_str()),
    ];
    let response = state
        .client()
        .post(RECOVER_ALBUM_URL)
        .query(&[("g_tk", auth.g_tk.to_string())])
        .header(ACCEPT, "*/*")
        .header(
            ACCEPT_LANGUAGE,
            "zh-CN,zh;q=0.9,en;q=0.8,en-GB;q=0.7,en-US;q=0.6,zh-TW;q=0.5",
        )
        .header(CACHE_CONTROL, "no-cache")
        .header(PRAGMA, "no-cache")
        .header(ORIGIN, "https://user.qzone.qq.com")
        .header(REFERER, format!("https://user.qzone.qq.com/{}", auth.uin))
        .header(USER_AGENT, &auth.user_agent)
        .header(COOKIE, &auth.cookie_header)
        .header("priority", "u=1, i")
        .header("sec-fetch-dest", "empty")
        .header("sec-fetch-mode", "cors")
        .header("sec-fetch-site", "same-origin")
        .header("content-type", "application/x-www-form-urlencoded;charset=UTF-8")
        .form(&form)
        .send()
        .await
        .map_err(|error| format!("恢复相册失败：{error}"))?;
    let status = response.status();
    let text = response.text().await.map_err(|error| format!("读取恢复相册响应失败：{error}"))?;
    if !status.is_success() {
        return Err(format!("恢复相册失败：HTTP {status}"));
    }
    let parsed = ensure_qzone_success(parse_qzone_action_response(&text)?)?;
    let data = parsed.get("data").cloned().unwrap_or_default();
    let succeeded = data.get("succ_num").and_then(Value::as_u64).unwrap_or(0);
    let failed = data.get("fail_num").and_then(Value::as_u64).unwrap_or(0);
    if succeeded != 1 || failed != 0 {
        return Err(format!("相册恢复未完成：成功 {succeeded} 个，失败 {failed} 个"));
    }
    Ok(parsed)
}

#[tauri::command]
pub async fn recover_recycle_photos(
    state: tauri::State<'_, QLoginState>,
    pwd2sig: String,
    source_album_id: String,
    target_album_id: String,
    photo_ids: Vec<String>,
) -> Result<Value, String> {
    if photo_ids.is_empty() {
        return Err("请先选择需要恢复的照片".into());
    }
    let auth = state.qzone_auth().await?;
    if source_album_id.trim().is_empty() { return Err("照片缺少回收站来源相册 ID".into()); }
    if target_album_id.trim().is_empty() { return Err("照片缺少恢复目标相册 ID".into()); }
    let pic_list = format!("{}@{}", source_album_id, photo_ids.join("_"));
    let g_tk = auth.g_tk.to_string();
    let qzreferrer = format!("https://user.qzone.qq.com/{}", auth.uin);
    let form = vec![
        ("uin", auth.uin.as_str()),
        ("hostUin", auth.uin.as_str()),
        // Destination album and recycle-bin source group are different IDs.
        ("albumId", target_album_id.as_str()),
        ("picList", pic_list.as_str()),
            ("pwd2sig", pwd2sig.as_str()),
            ("format", "fs"),
            ("inCharset", "utf-8"),
            ("outCharset", "utf-8"),
            ("notice", "0"),
            ("callbackFun", "_Callback"),
            ("plat", "qzone"),
            ("source", "qzone"),
            ("appid", "4"),
            ("qzreferrer", qzreferrer.as_str()),
    ];
    let response = state
        .client()
        .post(RECOVER_PHOTO_URL)
        .query(&[("g_tk", g_tk.as_str())])
        .header(ACCEPT, "*/*")
        .header(
            ACCEPT_LANGUAGE,
            "zh-CN,zh;q=0.9,en;q=0.8,en-GB;q=0.7,en-US;q=0.6,zh-TW;q=0.5",
        )
        .header(CACHE_CONTROL, "no-cache")
        .header(PRAGMA, "no-cache")
        .header(REFERER, format!("https://user.qzone.qq.com/{}", auth.uin))
        .header(USER_AGENT, &auth.user_agent)
        .header(COOKIE, &auth.cookie_header)
        .header(ORIGIN, "https://user.qzone.qq.com")
        .header("priority", "u=1, i")
        .header("sec-fetch-dest", "empty")
        .header("sec-fetch-mode", "cors")
        .header("sec-fetch-site", "same-origin")
        .header("content-type", "application/x-www-form-urlencoded;charset=UTF-8")
        .form(&form)
        .send()
        .await
        .map_err(|error| format!("恢复照片失败：{error}"))?;
    let status = response.status();
    let text = response.text().await.map_err(|error| error.to_string())?;
    if !status.is_success() {
        let detail = text.chars().take(300).collect::<String>();
        return Err(format!("恢复照片失败：HTTP {status} {detail}"));
    }
    let parsed = ensure_qzone_success(parse_qzone_action_response(&text)?)?;
    if let Some(succeeded) = parsed
        .get("data")
        .and_then(|data| data.get("succ_num"))
        .and_then(Value::as_u64)
    {
        let expected = photo_ids.len() as u64;
        if succeeded != expected {
            let failed = parsed
                .get("data")
                .and_then(|data| data.get("fail_num"))
                .and_then(Value::as_u64)
                .unwrap_or(expected.saturating_sub(succeeded));
            return Err(format!(
                "照片恢复未完成：请求 {expected} 张，成功 {succeeded} 张，失败 {failed} 张"
            ));
        }
    }
    Ok(parsed)
}

fn retryable_response_reason(status: reqwest::StatusCode, body: &str) -> Option<String> {
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        return Some(format!("HTTP {status}"));
    }
    if !status.is_success() {
        return None;
    }
    let value = match serde_json::from_str::<Value>(body) {
        Ok(value) => value,
        Err(_) => return Some("响应不是有效 JSON".into()),
    };
    if let Some(code) = value.get("code").and_then(Value::as_i64) {
        if code != 0 {
            let message = value
                .get("message")
                .or_else(|| value.get("msg"))
                .and_then(Value::as_str)
                .unwrap_or("未知错误");
            let permanent = ["未登录", "登录失效", "权限", "封禁", "禁止访问", "p_skey"]
                .iter()
                .any(|keyword| message.contains(keyword));
            return (!permanent).then(|| format!("接口错误 {code}：{message}"));
        }
    }
    if value.get("data").is_none() {
        return Some("响应中暂时缺少 data".into());
    }
    None
}

fn feed_retry_delay(attempt: u32) -> std::time::Duration {
    std::time::Duration::from_millis(1_500 * 2_u64.pow(attempt.saturating_sub(1)))
}

fn sec_ch_ua(user_agent: &str) -> String {
    if let Some(start) = user_agent.find("Chrome/") {
        let version_start = start + 7;
        let major = user_agent[version_start..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>();
        let version = if major.is_empty() { "131" } else { &major };
        format!("\"Not;A=Brand\";v=\"8\", \"Chromium\";v=\"{version}\", \"Microsoft Edge\";v=\"{version}\"")
    } else {
        "\"Not;A=Brand\";v=\"8\", \"Apple\";v=\"0\", \"Safari\";v=\"18\"".to_owned()
    }
}

fn sec_platform(user_agent: &str) -> &'static str {
    if user_agent.contains("iPhone") {
        "\"iOS\""
    } else {
        "\"Android\""
    }
}
fn response_headers(headers: &reqwest::header::HeaderMap) -> Vec<Value> {
    headers
        .iter()
        .map(|(name, value)| {
            json!({
                "name": name.as_str(),
                "value": String::from_utf8_lossy(value.as_bytes()),
            })
        })
        .collect()
}

fn log_feed_request_error(
    stage: &str,
    request_url: &str,
    query: &[(&str, String)],
    user_agent: &str,
    status: Option<reqwest::StatusCode>,
    headers: Option<&reqwest::header::HeaderMap>,
    response_body: Option<&str>,
    attempts: &[String],
    error: &str,
) {
    let parameters = query
        .iter()
        .map(|(name, value)| ((*name).to_owned(), Value::String(value.clone())))
        .collect::<serde_json::Map<String, Value>>();
    let parsed_body = response_body.and_then(|text| serde_json::from_str::<Value>(text).ok());
    let body = match (response_body, parsed_body) {
        (_, Some(value)) => Some(value),
        (Some(text), None) => Some(json!({
            "format": "raw",
            "bytesReceived": text.as_bytes().len(),
            "content": "非完整 JSON 或非 JSON 响应，原始正文见本诊断块下方"
        })),
        (None, None) => None,
    };
    let diagnostic = json!({
        "event": "qzone_archive_request_error",
        "stage": stage,
        "error": error,
        "request": {
            "method": "GET",
            "url": request_url,
            "parameters": parameters,
            "headers": {
                "Accept": "application/json",
                "Accept-Encoding": "gzip, deflate, br",
                "Accept-Language": "zh-CN,zh;q=0.9,en;q=0.8,en-GB;q=0.7,en-US;q=0.6,zh-TW;q=0.5",
                "Cache-Control": "no-cache",
                "Pragma": "no-cache",
                "Origin": "https://h5.qzone.qq.com",
                "Referer": "https://h5.qzone.qq.com/",
                "Sec-Fetch-Dest": "empty",
                "Sec-Fetch-Mode": "cors",
                "Sec-Fetch-Site": "same-site",
                "Sec-Ch-Ua-Mobile": "?1",
                "User-Agent": user_agent,
                "Cookie": "[已隐藏：登录凭证不会写入控制台]"
            }
        },
        "response": {
            "status": status.map(|value| value.as_u16()),
            "statusText": status.and_then(|value| value.canonical_reason()),
            "headers": headers.map(response_headers),
            "body": body,
        },
        "transportAttempts": attempts,
    });
    let formatted =
        serde_json::to_string_pretty(&diagnostic).unwrap_or_else(|_| diagnostic.to_string());
    eprintln!("\n================ QZONE ARCHIVE REQUEST ERROR ================\n{formatted}");
    if let Some(text) = response_body {
        eprintln!("---------------- RAW RESPONSE BODY ----------------\n{text}\n---------------- END RAW RESPONSE BODY ----------------");
    }
    eprintln!("================ END QZONE ARCHIVE REQUEST ERROR ================\n");
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedPage {
    pub(crate) feeds: Vec<Value>,
    pub(crate) attach_info: Option<String>,
    pub(crate) has_more: bool,
}

#[derive(Debug)]
pub(crate) struct VisibleMomentPage {
    pub(crate) feeds: Vec<Value>,
    pub(crate) moment_count: usize,
    pub(crate) total: u64,
    pub(crate) next_pos: u32,
    pub(crate) has_more: bool,
}

#[derive(Debug)]
pub(crate) struct HistoryMessagePage {
    pub(crate) feeds: Vec<Value>,
    pub(crate) record_count: usize,
}

fn value_text(value: &Value, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        value.get(*name).and_then(|value| match value {
            Value::String(text) if !text.trim().is_empty() => Some(text.clone()),
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        })
    })
}

fn value_number(value: &Value, names: &[&str]) -> i64 {
    names
        .iter()
        .find_map(|name| {
            value.get(*name).and_then(|value| {
                value
                    .as_i64()
                    .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
            })
        })
        .unwrap_or(0)
}

fn normalized_moment_pictures(moment: &Value) -> Option<Value> {
    let pictures = moment.get("pic")?.as_array()?;
    let pictures = pictures
        .iter()
        .filter_map(|picture| {
            let mut seen = std::collections::HashSet::new();
            let urls = ["url3", "url2", "url1", "rawUrl", "origin_url", "smallurl"]
                .iter()
                .filter_map(|name| value_text(picture, &[*name]))
                .filter(|url| seen.insert(url.clone()))
                .map(|url| json!({ "url": url }))
                .collect::<Vec<_>>();
            (!urls.is_empty()).then(|| json!({ "photourl": urls }))
        })
        .collect::<Vec<_>>();
    (!pictures.is_empty()).then(|| json!({ "picdata": { "pic": pictures } }))
}

fn normalized_moment_video(moment: &Value) -> Option<Value> {
    let from_video_list = moment
        .get("video")
        .and_then(Value::as_array)
        .and_then(|videos| videos.first());
    let from_picture = moment
        .get("pic")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find_map(|picture| picture.get("video_info"));
    let video = from_video_list.or(from_picture)?;
    let url = value_text(video, &["url3", "url2", "url", "video_url"])?;
    let cover = value_text(video, &["url1", "cover", "cover_url"])
        .or_else(|| from_video_list.and_then(|value| value_text(value, &["url1"])));
    let mut result = json!({ "videourl": url });
    if let Some(cover) = cover {
        result["coverurl"] = json!([{ "url": cover }]);
    }
    Some(result)
}

fn append_normalized_replies(
    comments: &[Value],
    reply_to_uin: Option<&str>,
    reply_to_name: Option<&str>,
    replies: &mut Vec<Value>,
) {
    for comment in comments {
        let uin = value_text(comment, &["uin", "fuin", "user_uin"]);
        let name = value_text(comment, &["name", "nick", "nickname"]);
        let content = value_text(comment, &["content", "con"]).unwrap_or_default();
        let created_at = value_number(comment, &["create_time", "created_time", "date"]);
        replies.push(json!({
            "user": { "uin": uin, "nickname": name },
            "replyuser": { "uin": reply_to_uin, "nickname": reply_to_name },
            "content": content,
            "date": created_at,
        }));
        if let Some(children) = comment.get("list_3").and_then(Value::as_array) {
            append_normalized_replies(children, uin.as_deref(), name.as_deref(), replies);
        }
    }
}

fn visible_moment_as_feeds(moment: &Value, owner_uin: &str, index: usize) -> Vec<Value> {
    let tid = value_text(moment, &["tid", "id"])
        .unwrap_or_else(|| format!("visible-{}-{index}", value_number(moment, &["created_time"])));
    let created_at = value_number(moment, &["created_time", "create_time", "date"]);
    let author_uin = value_text(moment, &["uin", "owner_uin"]).unwrap_or_else(|| owner_uin.into());
    let author_name = value_text(moment, &["name", "nickname"]);
    let content = value_text(moment, &["content", "con"]).unwrap_or_default();
    let mut original = json!({
        "cell_id": { "cellid": tid },
        "cell_comm": {
            "appid": 311,
            "feedskey": format!("311_{author_uin}_{tid}"),
            "time": created_at,
        },
        "cell_summary": { "summary": content },
        "cell_userinfo": { "user": { "uin": author_uin, "nickname": author_name } },
    });
    if let Some(pictures) = normalized_moment_pictures(moment) {
        original["cell_pic"] = pictures;
    }
    if let Some(video) = normalized_moment_video(moment) {
        original["cell_video"] = video;
    }
    let mut feeds = vec![json!({
        "comm": { "subid": 0, "time": created_at, "feedskey": format!("visible:{tid}") },
        "userinfo": { "user": { "uin": author_uin, "nickname": author_name } },
        "original": original,
    })];

    if let Some(comments) = moment.get("commentlist").and_then(Value::as_array) {
        for (comment_index, comment) in comments.iter().enumerate() {
            let comment_uin = value_text(comment, &["uin", "fuin", "user_uin"]);
            let comment_name = value_text(comment, &["name", "nick", "nickname"]);
            let comment_content = value_text(comment, &["content", "con"]).unwrap_or_default();
            let comment_time = value_number(comment, &["create_time", "created_time", "date"]);
            let comment_id = value_text(comment, &["commentid", "comment_id", "id"])
                .unwrap_or_else(|| format!("visible:{tid}:{comment_index}"));
            let mut replies = Vec::new();
            if let Some(children) = comment.get("list_3").and_then(Value::as_array) {
                append_normalized_replies(
                    children,
                    comment_uin.as_deref(),
                    comment_name.as_deref(),
                    &mut replies,
                );
            }
            let mut comment_original = original.clone();
            comment_original["cell_comment"] = json!({
                "main_comment": {
                    "commentid": comment_id,
                    "user": { "uin": comment_uin, "nickname": comment_name },
                    "content": comment_content,
                    "date": comment_time,
                    "replynum": replies.len(),
                    "replys": replies,
                }
            });
            feeds.push(json!({
                "comm": {
                    "subid": 2,
                    "time": comment_time,
                    "feedskey": format!("visible-comment:{tid}:{comment_id}"),
                },
                "userinfo": { "user": { "uin": comment_uin, "nickname": comment_name } },
                "summary": { "summary": comment_content },
                "original": comment_original,
            }));
        }
    }

    if let Some(likes) = moment.get("__like").and_then(Value::as_array) {
        for (like_index, like) in likes.iter().enumerate() {
            let like_uin = value_text(like, &["fuin", "uin"]);
            let like_name = value_text(like, &["nick", "name", "nickname"]);
            let like_key = like_uin
                .clone()
                .unwrap_or_else(|| format!("index-{like_index}"));
            feeds.push(json!({
                "comm": {
                    "subid": 217,
                    "time": created_at,
                    "feedskey": format!("visible-like:{tid}:{like_key}"),
                },
                "userinfo": { "user": { "uin": like_uin, "nickname": like_name } },
                "original": original,
            }));
        }
    }
    feeds
}

pub(crate) async fn fetch_visible_moments(
    state: &QLoginState,
    pos: u32,
    num: u32,
) -> Result<VisibleMomentPage, String> {
    let auth = state.qzone_auth().await?;
    // This legacy endpoint only paginates reliably with at most 30 records.
    // Its offset advances by the requested page size, not by msglist.len().
    let num = num.clamp(1, 30);
    let query = [
        ("uin", auth.uin.clone()),
        ("ftype", "0".into()),
        ("sort", "0".into()),
        ("pos", pos.to_string()),
        ("num", num.to_string()),
        ("replynum", "100".into()),
        ("g_tk", auth.g_tk.to_string()),
        ("callback", "_preloadCallback".into()),
        ("code_version", "1".into()),
        ("format", "jsonp".into()),
        ("need_private_comment", "1".into()),
    ];
    let mut last_error = String::new();
    for attempt in 1..=FEED_RESPONSE_ATTEMPTS {
        let response = state
            .client()
            .get(OWN_MOMENTS_URL)
            .header(ACCEPT, "*/*")
            .header(ACCEPT_LANGUAGE, "en-US,en;q=0.9")
            .header(REFERER, format!("https://user.qzone.qq.com/{}/main", auth.uin))
            .header(USER_AGENT, DESKTOP_QZONE_USER_AGENT)
            .header(COOKIE, &auth.desktop_cookie_header)
            .header("Priority", "u=1, i")
            .header(
                "Sec-Ch-Ua",
                "\"Not;A=Brand\";v=\"24\", \"Chromium\";v=\"128\"",
            )
            .header("Sec-Ch-Ua-Mobile", "?0")
            .header("Sec-Ch-Ua-Platform", "\"Linux\"")
            .header("Sec-Fetch-Dest", "empty")
            .header("Sec-Fetch-Mode", "cors")
            .header("Sec-Fetch-Site", "same-origin")
            .query(&query)
            .send()
            .await;
        match response {
            Ok(response) => {
                let status = response.status();
                let body = response
                    .text()
                    .await
                    .map_err(|error| format!("读取本人说说响应失败：{error}"))?;
                if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                    last_error = format!("HTTP {status}");
                } else if !status.is_success() {
                    return Err(format!("获取本人说说失败：HTTP {status}"));
                } else {
                    let value = ensure_qzone_success(parse_qzone_json(&body)?)?;
                    let moments = value
                        .get("msglist")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let total = value
                        .get("total")
                        .and_then(|value| {
                            value
                                .as_u64()
                                .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
                        })
                        .unwrap_or((pos as usize + moments.len()) as u64);
                    let moment_count = moments.len();
                    let feeds = moments
                        .iter()
                        .enumerate()
                        .flat_map(|(index, moment)| visible_moment_as_feeds(moment, &auth.uin, index))
                        .collect();
                    let next_pos = pos.saturating_add(num);
                    return Ok(VisibleMomentPage {
                        feeds,
                        moment_count,
                        total,
                        next_pos,
                        has_more: u64::from(next_pos) < total,
                    });
                }
            }
            Err(error) => last_error = error.to_string(),
        }
        if attempt < FEED_RESPONSE_ATTEMPTS {
            tokio::time::sleep(feed_retry_delay(attempt)).await;
        }
    }
    Err(format!("获取本人说说失败：{last_error}"))
}

fn trailing_qq_number(value: &str) -> Option<String> {
    let digits = value
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    (!digits.is_empty()).then_some(digits)
}

fn current_utc_year() -> i32 {
    let mut days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 86_400;
    let mut year = 1970_i32;
    loop {
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let year_days = if leap { 366 } else { 365 };
        if days < year_days {
            return year;
        }
        days -= year_days;
        year += 1;
    }
}

fn history_date_to_timestamp(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> i64 {
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return 0;
    }
    // Howard Hinnant's civil-date conversion. QQ displays these timestamps in
    // China Standard Time, so convert the wall clock from UTC+8 to Unix time.
    let adjusted_year = year - i32::from(month <= 2);
    let era = if adjusted_year >= 0 {
        adjusted_year
    } else {
        adjusted_year - 399
    } / 400;
    let year_of_era = adjusted_year - era * 400;
    let shifted_month = month as i32 + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day as i32 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era as i64 * 146_097 + day_of_era as i64 - 719_468;
    days * 86_400 + i64::from(hour * 3_600 + minute * 60 + second) - 8 * 3_600
}

fn parse_history_timestamp(value: &str) -> i64 {
    for name in ["data-time", "data-timestamp", "data-abstime"] {
        let pattern = format!(r#"{name}=[\"'](\d{{9,13}})[\"']"#);
        if let Some(raw) = regex::Regex::new(&pattern)
            .ok()
            .and_then(|pattern| pattern.captures(value))
            .and_then(|captures| captures.get(1))
            .and_then(|number| number.as_str().parse::<i64>().ok())
        {
            return if raw > 10_000_000_000 { raw / 1_000 } else { raw };
        }
    }
    let full = regex::Regex::new(
        r"(?P<year>\d{4})[年/-](?P<month>\d{1,2})[月/-](?P<day>\d{1,2})日?\s+(?P<hour>\d{1,2}):(?P<minute>\d{1,2})(?::(?P<second>\d{1,2}))?",
    )
    .expect("fixed history timestamp regex");
    let short = regex::Regex::new(
        r"(?P<month>\d{1,2})月(?P<day>\d{1,2})日\s+(?P<hour>\d{1,2}):(?P<minute>\d{1,2})(?::(?P<second>\d{1,2}))?",
    )
    .expect("fixed short history timestamp regex");
    let captures = full.captures(value).or_else(|| short.captures(value));
    let Some(captures) = captures else { return 0; };
    let number = |name: &str| {
        captures
            .name(name)
            .and_then(|value| value.as_str().parse::<u32>().ok())
    };
    let year = captures
        .name("year")
        .and_then(|value| value.as_str().parse::<i32>().ok())
        .unwrap_or_else(current_utc_year);
    history_date_to_timestamp(
        year,
        number("month").unwrap_or(1),
        number("day").unwrap_or(1),
        number("hour").unwrap_or(0),
        number("minute").unwrap_or(0),
        number("second").unwrap_or(0),
    )
}

fn history_record_hash(parts: &[&str]) -> u64 {
    parts
        .iter()
        .flat_map(|part| part.bytes().chain(std::iter::once(0xff)))
        .fold(0xcbf29ce484222325, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        })
}

fn decode_history_html(response: &str) -> Result<String, String> {
    let capture = regex::Regex::new(r"(?s)html\s*:\s*'(.*)',\s*opuin")
        .expect("fixed history response regex")
        .captures(response)
        .and_then(|captures| captures.get(1))
        .ok_or("历史消息响应中缺少 html 数据")?;
    let hex = regex::Regex::new(r"\\x([0-9a-fA-F]{2})").expect("fixed hex escape regex");
    let decoded = hex.replace_all(capture.as_str(), |captures: &regex::Captures<'_>| {
        u8::from_str_radix(&captures[1], 16)
            .map(char::from)
            .unwrap_or('\u{fffd}')
            .to_string()
    });
    Ok(decoded
        .replace("\\/", "/")
        .replace("\\'", "'")
        .replace("\\\"", "\"")
        .replace("\\t", " ")
        .replace("\\r", " ")
        .replace("\\n", "\n")
        .replace("\\\\", "\\"))
}

fn history_attribute(markup: &str, name: &str) -> Option<String> {
    let pattern = regex::Regex::new(&format!(
        r#"(?is)\b{}\s*=\s*[\"']([^\"']*)[\"']"#,
        regex::escape(name)
    ))
    .ok()?;
    pattern
        .captures(markup)
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str().to_owned())
}

fn history_plain_text(markup: &str) -> String {
    let without_tags = regex::Regex::new(r"(?is)<[^>]+>")
        .expect("fixed HTML tag regex")
        .replace_all(markup, " ");
    let decoded = without_tags
        .replace("&nbsp;", " ")
        .replace("&#160;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    decoded
        .replace("\\t", " ")
        .replace("\\r", " ")
        .replace("\\n", "\n")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn history_elements(markup: &str, tag: &str, class_name: &str) -> Vec<(String, String)> {
    let pattern = regex::Regex::new(&format!(
        r#"(?is)<{tag}\b(?P<attrs>[^>]*)>(?P<body>.*?)</{tag}>"#
    ))
    .expect("fixed history element regex");
    pattern
        .captures_iter(markup)
        .filter_map(|captures| {
            let attrs = captures.name("attrs")?.as_str();
            let classes = history_attribute(attrs, "class")?;
            classes
                .split_whitespace()
                .any(|class| class == class_name)
                .then(|| {
                    (
                        attrs.to_owned(),
                        captures
                            .name("body")
                            .map(|body| body.as_str().to_owned())
                            .unwrap_or_default(),
                    )
                })
        })
        .collect()
}

fn history_element(markup: &str, tag: &str, class_name: &str) -> Option<(String, String)> {
    history_elements(markup, tag, class_name).into_iter().next()
}

fn history_event_key(value: &str) -> Option<(String, i64, i64, i64)> {
    let captures = regex::Regex::new(r"^fct_(\d+)_(\d+)_(\d+)_(\d+)(?:_|$)")
        .expect("fixed history event key regex")
        .captures(value)?;
    Some((
        captures.get(1)?.as_str().to_owned(),
        captures.get(2)?.as_str().parse().ok()?,
        captures.get(3)?.as_str().parse().ok()?,
        captures.get(4)?.as_str().parse().ok()?,
    ))
}

fn history_content_media(card_body: &str) -> Vec<String> {
    let image_pattern = regex::Regex::new(r"(?is)<img\b(?P<attrs>[^>]*)>")
        .expect("fixed history image regex");
    let mut urls = history_elements(card_body, "a", "img-item")
        .into_iter()
        .flat_map(|(_, body)| {
            image_pattern
                .captures_iter(&body)
                .filter_map(|image| image.name("attrs"))
                .filter_map(|attrs| {
                    history_attribute(attrs.as_str(), "src")
                        .or_else(|| history_attribute(attrs.as_str(), "data-src"))
                })
                .collect::<Vec<_>>()
        })
        .map(|url| {
            if url.starts_with("//") {
                format!("https:{url}")
            } else {
                url
            }
        })
        .filter(|url| {
            let lower = url.to_ascii_lowercase();
            url.starts_with("http")
                && !lower.contains("qlogo")
                && !lower.contains("headimg")
                && !lower.contains("qzonestyle")
                && !lower.contains("custompraise")
                && (lower.contains("qpic.cn") || lower.contains("photo.store.qq.com"))
        })
        .collect::<Vec<_>>();
    urls.sort();
    urls.dedup();
    urls
}

fn history_mood_cell_id(card_body: &str, owner_uin: &str) -> Option<String> {
    let pattern = regex::Regex::new(
        r#"(?i)(?:https?:)?//user\.qzone\.qq\.com/(\d+)/mood/([^?&"'<>/\s]+)"#,
    )
    .expect("fixed Qzone mood URL regex");
    let cell_id = pattern.captures_iter(card_body).find_map(|captures| {
        (captures.get(1)?.as_str() == owner_uin)
            .then(|| captures.get(2).map(|value| value.as_str().to_owned()))
            .flatten()
    });
    cell_id
}

fn history_original_content(content: &str, owner_name: Option<&str>) -> Option<String> {
    let mut content = content.trim().to_owned();
    let owner_name = owner_name.map(str::trim).filter(|name| !name.is_empty())?;
    if !content.starts_with(owner_name) {
        return None;
    }
    content = content[owner_name.len()..]
        .trim_start_matches(|character: char| {
            character.is_whitespace()
                || matches!(character, ':' | '：' | '，' | ',' | '。' | '·' | '-')
        })
        .trim()
        .to_owned();
    for marker in ["的主页", "的说说"] {
        if let Some(rest) = content.strip_prefix(marker) {
            content = rest
                .trim_start_matches(|character: char| {
                    character.is_whitespace()
                        || matches!(character, ':' | '：' | '，' | ',' | '。' | '·' | '-')
                })
                .trim()
                .to_owned();
        }
    }
    (!content.is_empty()).then_some(content)
}

fn history_html_as_feeds(
    html: &str,
    owner_uin: &str,
    owner_name: Option<&str>,
    _offset: u32,
) -> (usize, Vec<Value>) {
    let card_pattern = regex::Regex::new(r"(?is)<li\b(?P<attrs>[^>]*)>(?P<body>.*?)</li>")
        .expect("fixed history card regex");
    let cards = card_pattern
        .captures_iter(html)
        .filter(|card| {
            card.name("attrs")
                .and_then(|attrs| history_attribute(attrs.as_str(), "class"))
                .is_some_and(|classes| {
                    ["f-single", "f-s-s"].iter().all(|required| {
                        classes.split_whitespace().any(|class| class == *required)
                    })
                })
        })
        .collect::<Vec<_>>();
    let scanned_count = cards.len();
    let feeds = cards
        .into_iter()
        .filter_map(|card| {
            let card_attrs = card.name("attrs")?.as_str();
            let card_body = card.name("body")?.as_str();
            let stable_id = history_attribute(card_attrs, "data-key")
                .or_else(|| history_attribute(card_attrs, "id"))?;
            let (actor_uin, family, subtype, key_time) = history_event_key(&stable_id)?;
            let event_type = match family {
                217 => 217,
                311 => 2,
                _ => return None,
            };
            let people = history_elements(card_body, "a", "q_namecard")
                .into_iter()
                .filter_map(|(attrs, body)| {
                    let uin = ["link", "data-uin", "href"].iter().find_map(|name| {
                        history_attribute(&attrs, name)
                            .and_then(|value| trailing_qq_number(&value))
                    })?;
                    let name = history_plain_text(&body);
                    Some((uin, name))
                })
                .collect::<Vec<_>>();
            let actor_name = people
                .iter()
                .find(|(uin, _)| uin == &actor_uin)
                .map(|(_, name)| name.clone())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| actor_uin.clone());
            let (_, content_markup) = history_element(card_body, "p", "txt-box-title")?;
            let content = history_original_content(&history_plain_text(&content_markup), owner_name)?;
            let time_node = history_element(card_body, "div", "info-detail");
            let time_text = time_node
                .as_ref()
                .map(|(_, body)| history_plain_text(body))
                .unwrap_or_default();
            let time_markup = time_node
                .as_ref()
                .map(|(attrs, _)| attrs.as_str())
                .unwrap_or_default();
            let parsed_time = parse_history_timestamp(&format!("{time_markup} {time_text}"));
            let event_time = if key_time > 0 { key_time } else { parsed_time };
            let media_urls = history_content_media(card_body);
            let pictures = media_urls
                .iter()
                .map(|url| json!({ "photourl": [{ "url": url }] }))
                .collect::<Vec<_>>();
            let joined_media = media_urls.join("\n");
            let cell_id = history_mood_cell_id(card_body, owner_uin).unwrap_or_else(|| {
                format!(
                    "history-v2:{:016x}",
                    history_record_hash(&[owner_uin, &content, &joined_media])
                )
            });
            let target_name = people
                .iter()
                .find(|(uin, _)| uin != &actor_uin && uin != owner_uin)
                .map(|(_, name)| name.as_str());
            let event_summary = match event_type {
                217 => "点赞了这条说说".to_owned(),
                _ if subtype == 14 || subtype == 35 => target_name
                    .map(|name| format!("回复了 {name}（旧历史接口未保留回复正文）"))
                    .unwrap_or_else(|| "历史回复（旧历史接口未保留回复正文）".to_owned()),
                _ => "历史评论（旧历史接口未保留评论正文）".to_owned(),
            };
            let comments = (event_type == 2).then(|| {
                json!({
                    "main_comment": {
                        "commentid": stable_id,
                        "content": event_summary,
                        "date": event_time,
                        "user": { "uin": actor_uin, "nickname": actor_name },
                        "replys": [],
                    }
                })
            });
            Some(json!({
                "comm": {
                    "subid": event_type,
                    "time": event_time,
                    "feedskey": format!("history-v2-event:{stable_id}"),
                },
                "userinfo": { "user": { "uin": actor_uin, "nickname": actor_name } },
                "summary": { "summary": event_summary },
                "original": {
                    "cell_id": { "cellid": cell_id },
                    "cell_comm": {
                        "appid": 311,
                        "time": 0,
                        "feedskey": format!("history-v2-original:{:016x}", history_record_hash(&[owner_uin, &content, &joined_media])),
                    },
                    "cell_userinfo": { "user": { "uin": owner_uin, "nickname": owner_name } },
                    "cell_summary": { "summary": content },
                    "cell_pic": { "picdata": { "pic": pictures } },
                    "cell_comment": comments,
                },
            }))
        })
        .collect();
    (scanned_count, feeds)
}

pub(crate) async fn fetch_history_messages(
    state: &QLoginState,
    offset: u32,
    count: u32,
    owner_name: Option<&str>,
) -> Result<HistoryMessagePage, String> {
    let auth = state.qzone_auth().await?;
    let count = count.clamp(1, 30);
    let query = vec![
        ("uin", auth.uin.clone()),
        ("begin_time", "0".into()),
        ("end_time", "0".into()),
        ("getappnotification", "1".into()),
        ("getnotifi", "1".into()),
        ("has_get_key", "0".into()),
        ("offset", offset.to_string()),
        ("set", "0".into()),
        ("count", count.to_string()),
        ("useutf8", "1".into()),
        ("outputhtmlfeed", "1".into()),
        ("scope", "1".into()),
        ("format", "jsonp".into()),
        ("g_tk", auth.g_tk.to_string()),
        ("g_tk", auth.g_tk.to_string()),
    ];
    let mut last_error = String::new();
    for attempt in 1..=FEED_RESPONSE_ATTEMPTS {
        let response = state
            .client()
            .get(HISTORY_MESSAGES_URL)
            .header(
                ACCEPT,
                "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8",
            )
            .header(ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8,en-US;q=0.6")
            .header(CACHE_CONTROL, "no-cache")
            .header(PRAGMA, "no-cache")
            .header(REFERER, format!("https://user.qzone.qq.com/{}/main", auth.uin))
            .header(USER_AGENT, DESKTOP_QZONE_USER_AGENT)
            .header(COOKIE, &auth.desktop_cookie_header)
            .header("Sec-Ch-Ua", "\"Not A(Brand\";v=\"99\", \"Chromium\";v=\"128\"")
            .header("Sec-Ch-Ua-Mobile", "?0")
            .header("Sec-Ch-Ua-Platform", "\"Linux\"")
            .header("Sec-Fetch-Dest", "document")
            .header("Sec-Fetch-Mode", "navigate")
            .header("Sec-Fetch-Site", "same-origin")
            .header("Upgrade-Insecure-Requests", "1")
            .query(&query)
            .send()
            .await;
        match response {
            Ok(response) => {
                let status = response.status();
                let body = response
                    .text()
                    .await
                    .map_err(|error| format!("读取历史消息响应失败：{error}"))?;
                if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                    last_error = format!("HTTP {status}");
                } else if !status.is_success() {
                    return Err(format!("获取历史消息失败：HTTP {status}"));
                } else if !body.contains("html") {
                    return Err(format!(
                        "历史消息接口返回异常：{}",
                        body.chars().take(120).collect::<String>()
                    ));
                } else {
                    let html = decode_history_html(&body)?;
                    let (record_count, feeds) =
                        history_html_as_feeds(&html, &auth.uin, owner_name, offset);
                    return Ok(HistoryMessagePage {
                        record_count,
                        feeds,
                    });
                }
            }
            Err(error) => last_error = error.to_string(),
        }
        if attempt < FEED_RESPONSE_ATTEMPTS {
            tokio::time::sleep(feed_retry_delay(attempt)).await;
        }
    }
    Err(format!("获取历史消息失败：{last_error}"))
}

fn parse_feed_page(value: Value) -> Result<FeedPage, String> {
    if let Some(code) = value.get("code").and_then(Value::as_i64) {
        if code != 0 {
            let message = value
                .get("message")
                .or_else(|| value.get("msg"))
                .and_then(Value::as_str)
                .unwrap_or("未知错误");
            return Err(format!("QQ 空间动态接口返回错误 {code}：{message}"));
        }
    }
    let data = value.get("data").ok_or("动态响应中缺少 data")?;
    let feeds = data
        .get("vFeeds")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let attach_info = data
        .get("attachinfo")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let server_has_more = data.get("hasmore").and_then(Value::as_i64).unwrap_or(0) != 0;
    let has_more = server_has_more && !feeds.is_empty() && attach_info.is_some();
    Ok(FeedPage {
        feeds,
        attach_info,
        has_more,
    })
}

pub(crate) async fn fetch_feeds(
    state: &QLoginState,
    refresh_type: &str,
    attach_info: Option<&str>,
) -> Result<FeedPage, String> {
    fetch_feeds_with_attempts(state, refresh_type, attach_info, FEED_RESPONSE_ATTEMPTS).await
}

pub(crate) async fn fetch_feeds_once(
    state: &QLoginState,
    refresh_type: &str,
    attach_info: Option<&str>,
) -> Result<FeedPage, String> {
    fetch_feeds_with_attempts(state, refresh_type, attach_info, 1).await
}

pub(crate) fn feed_error_can_skip(error: &str) -> bool {
    !feed_error_is_transient(error)
        && error.starts_with("QQ 空间动态接口返回错误")
}

pub(crate) fn feed_error_is_transient(error: &str) -> bool {
    error.starts_with("解析空间动态失败：")
        || error.starts_with("动态响应中缺少 data")
        || error.contains("HTTP 5")
        || error.contains("HTTP 429")
        || error.contains("请求超时")
        || error.contains("连接失败")
        || error.contains("传输失败")
        || error.contains("响应体读取失败")
        || ["系统繁忙", "稍后再试", "频繁", "限流"]
            .iter()
            .any(|keyword| error.contains(keyword))
}

async fn fetch_feeds_with_attempts(
    state: &QLoginState,
    refresh_type: &str,
    attach_info: Option<&str>,
    attempts: u32,
) -> Result<FeedPage, String> {
    let auth = state.qzone_auth().await?;
    let mut query = vec![
        ("g_tk", auth.g_tk.to_string()),
        ("res_type", "1".into()),
        ("refresh_type", refresh_type.into()),
        ("format", "json".into()),
    ];
    if let Some(attach_info) = attach_info {
        if attach_info.trim().is_empty() {
            let error = "分页游标不能为空";
            log_feed_request_error(
                "validate_request",
                FEEDS_URL,
                &query,
                &auth.user_agent,
                None,
                None,
                None,
                &[],
                error,
            );
            return Err(error.into());
        }
        query.push(("res_attach", attach_info.to_owned()));
    }
    let request_url = reqwest::Url::parse_with_params(FEEDS_URL, &query)
        .map(|url| url.to_string())
        .unwrap_or_else(|_| FEEDS_URL.to_owned());
    let client = state.client();
    let mut response = None;
    let mut last_error = None;
    let mut transport_attempts = Vec::new();
    let mut failed_response_status = None;
    let mut failed_response_headers = None;
    let mut failed_response_body = None;
    let mut last_attempt_logged = false;
    let attempts = attempts.max(1);
    for attempt in 1..=attempts {
        match client
            .get(FEEDS_URL)
            .header(ACCEPT, "application/json")
            .header(ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8,en-GB;q=0.7,en-US;q=0.6,zh-TW;q=0.5")
            .header(CACHE_CONTROL, "no-cache")
            .header(PRAGMA, "no-cache")
            .header(ORIGIN, "https://h5.qzone.qq.com")
            .header(REFERER, "https://h5.qzone.qq.com/")
            .header(USER_AGENT, &auth.user_agent)
            .header(COOKIE, &auth.cookie_header)
            .header("Sec-Fetch-Dest", "empty")
            .header("Sec-Fetch-Mode", "cors")
            .header("Sec-Fetch-Site", "same-site")
            .header("Sec-Ch-Ua", sec_ch_ua(&auth.user_agent))
            .header("Sec-Ch-Ua-Mobile", "?1")
            .header("Sec-Ch-Ua-Platform", sec_platform(&auth.user_agent))
            .query(&query)
            .send()
            .await
        {
            Ok(mut value) => {
                let status = value.status();
                let headers = value.headers().clone();
                let mut bytes = Vec::new();
                let mut read_error = None;
                loop {
                    match value.chunk().await {
                        Ok(Some(chunk)) => bytes.extend_from_slice(&chunk),
                        Ok(None) => break,
                        Err(reason) => {
                            read_error = Some(reason);
                            break;
                        }
                    }
                }
                let body = String::from_utf8_lossy(&bytes).into_owned();
                if let Some(reason) = read_error {
                    let detail = format!(
                        "响应体读取失败（第 {attempt}/{attempts} 次，已接收 {} 字节）：{reason:#}",
                        bytes.len()
                    );
                    transport_attempts.push(detail.clone());
                    last_error = Some(detail);
                    log_feed_request_error(
                        &format!("read_response_attempt_{attempt}"),
                        &request_url,
                        &query,
                        &auth.user_agent,
                        Some(status),
                        Some(&headers),
                        Some(&body),
                        &transport_attempts,
                        transport_attempts.last().expect("刚写入的重试错误应当存在"),
                    );
                    failed_response_status = Some(status);
                    failed_response_headers = Some(headers);
                    failed_response_body = Some(body);
                    last_attempt_logged = true;
                    if attempt < attempts {
                        tokio::time::sleep(feed_retry_delay(attempt)).await;
                    }
                } else {
                    if let Some(reason) = retryable_response_reason(status, &body) {
                        let detail = format!("{reason}（第 {attempt}/{attempts} 次）");
                        transport_attempts.push(detail.clone());
                        log_feed_request_error(
                            &format!("retryable_response_attempt_{attempt}"),
                            &request_url,
                            &query,
                            &auth.user_agent,
                            Some(status),
                            Some(&headers),
                            Some(&body),
                            &transport_attempts,
                            &detail,
                        );
                        if attempt < attempts {
                            tokio::time::sleep(feed_retry_delay(attempt)).await;
                            continue;
                        }
                    }
                    response = Some((status, headers, body));
                    break;
                }
            }
            Err(error) => {
                let kind = if error.is_timeout() {
                    "请求超时"
                } else if error.is_connect() {
                    "连接失败"
                } else {
                    "传输失败"
                };
                let detail = format!("{kind}（第 {attempt}/{attempts} 次）：{error:#}");
                transport_attempts.push(detail.clone());
                last_error = Some(detail);
                last_attempt_logged = false;
                if attempt < attempts {
                    tokio::time::sleep(feed_retry_delay(attempt)).await;
                }
            }
        }
    }
    let Some((status, headers, body)) = response else {
        let error = format!(
            "获取空间动态失败：{}",
            last_error.unwrap_or_else(|| "未知网络错误".into())
        );
        let stage = if failed_response_status.is_some() {
            "read_response"
        } else {
            "transport"
        };
        if !last_attempt_logged {
            log_feed_request_error(
                stage,
                &request_url,
                &query,
                &auth.user_agent,
                failed_response_status,
                failed_response_headers.as_ref(),
                failed_response_body.as_deref(),
                &transport_attempts,
                &error,
            );
        }
        return Err(error);
    };
    if !status.is_success() {
        let error = format!("获取空间动态失败：HTTP {status}");
        log_feed_request_error(
            "http_status",
            &request_url,
            &query,
            &auth.user_agent,
            Some(status),
            Some(&headers),
            Some(&body),
            &transport_attempts,
            &error,
        );
        return Err(error);
    }
    let value = match serde_json::from_str::<Value>(&body) {
        Ok(value) => value,
        Err(reason) => {
            let error = format!("解析空间动态失败：{reason}");
            log_feed_request_error(
                "parse_json",
                &request_url,
                &query,
                &auth.user_agent,
                Some(status),
                Some(&headers),
                Some(&body),
                &transport_attempts,
                &error,
            );
            return Err(error);
        }
    };
    match parse_feed_page(value) {
        Ok(page) => Ok(page),
        Err(error) => {
            log_feed_request_error(
                "parse_api_response",
                &request_url,
                &query,
                &auth.user_agent,
                Some(status),
                Some(&headers),
                Some(&body),
                &transport_attempts,
                &error,
            );
            Err(error)
        }
    }
}

#[tauri::command]
pub async fn fetch_first_feeds(state: tauri::State<'_, QLoginState>) -> Result<FeedPage, String> {
    fetch_feeds(&state, "1", None).await
}

#[tauri::command]
pub async fn fetch_more_feeds(
    state: tauri::State<'_, QLoginState>,
    attach_info: String,
) -> Result<FeedPage, String> {
    fetch_feeds(&state, "2", Some(&attach_info)).await
}

#[cfg(test)]
mod tests {
    use super::{
        decode_history_html, ensure_qzone_success, feed_error_can_skip,
        feed_error_is_transient, history_html_as_feeds, parse_feed_page, parse_qzone_json,
        retryable_response_reason, visible_moment_as_feeds, FEEDS_URL,
    };
    use reqwest::StatusCode;
    use serde_json::json;

    #[test]
    fn keeps_first_page_feeds_and_cursor() {
        let page = parse_feed_page(json!({
            "code": 0,
            "data": { "attachinfo": "next-cursor", "hasmore": 1, "vFeeds": [{"id": 1}] }
        }))
        .unwrap();
        assert_eq!(page.feeds.len(), 1);
        assert_eq!(page.attach_info.as_deref(), Some("next-cursor"));
        assert!(page.has_more);
    }

    #[test]
    fn empty_page_finishes_pagination() {
        let page = parse_feed_page(json!({"code": 0, "data": {"vFeeds": []}})).unwrap();
        assert!(page.feeds.is_empty());
        assert!(!page.has_more);
    }

    #[test]
    fn cursor_remains_server_encoded_until_query_serialization() {
        let cursor = "att=back%5Fserver%5Finfo%3Doffset%253D6&tl=123";
        let encoded =
            reqwest::Url::parse_with_params(FEEDS_URL, &[("res_attach", cursor)]).unwrap();
        assert!(encoded
            .as_str()
            .contains("back%255Fserver%255Finfo%253Doffset%25253D6%26tl%3D123"));
        assert_eq!(
            encoded
                .query_pairs()
                .find(|(key, _)| key == "res_attach")
                .unwrap()
                .1,
            cursor
        );
    }

    #[test]
    fn retries_rate_limits_and_temporary_api_errors() {
        assert!(retryable_response_reason(StatusCode::TOO_MANY_REQUESTS, "busy").is_some());
        assert!(retryable_response_reason(
            StatusCode::OK,
            r#"{"code":-1,"message":"系统繁忙，请稍后再试"}"#,
        )
        .is_some());
    }

    #[test]
    fn does_not_retry_expired_login_response() {
        assert!(retryable_response_reason(
            StatusCode::OK,
            r#"{"code":-3000,"message":"登录失效，请重新登录"}"#,
        )
        .is_none());
    }

    #[test]
    fn parses_qzone_callback_response() {
        let value = parse_qzone_json(r#"<script>frameElement.callback({"code":0,"data":{"succ_num":1}});</script>"#).unwrap();
        assert_eq!(value["code"], 0);
        assert_eq!(value["data"]["succ_num"], 1);
        assert!(ensure_qzone_success(value).is_ok());
    }

    #[test]
    fn parses_outer_object_from_nested_jsonp_response() {
        let value = parse_qzone_json(
            r#"shine0({"code":0,"data":{"albumList":[{"id":"album-1","name":"恢复相册"}]}});"#,
        )
        .unwrap();
        assert_eq!(value["code"], 0);
        assert_eq!(value["data"]["albumList"][0]["id"], "album-1");
    }

    #[test]
    fn rejects_response_without_code() {
        let value = serde_json::json!({"data": {"succ_num": 1}});
        assert!(ensure_qzone_success(value).is_err());
    }

    #[test]
    fn converts_visible_moment_comments_replies_and_likes() {
        let feeds = visible_moment_as_feeds(
            &json!({
                "tid": "moment-1",
                "uin": "10001",
                "name": "本人",
                "content": "历史说说",
                "created_time": 100,
                "pic": [{"url1": "https://example.com/photo.jpg"}],
                "commentlist": [{
                    "commentid": "comment-1",
                    "uin": "20001",
                    "name": "甲",
                    "content": "第一条评论",
                    "create_time": 110,
                    "list_3": [{
                        "uin": "30001",
                        "name": "乙",
                        "content": "回复甲",
                        "create_time": 120,
                        "list_3": [{
                            "uin": "20001",
                            "name": "甲",
                            "content": "回复乙",
                            "create_time": 130
                        }]
                    }]
                }],
                "__like": [{"fuin": "40001", "nick": "丙"}]
            }),
            "10001",
            0,
        );
        assert_eq!(feeds.len(), 3);
        assert_eq!(feeds[0]["original"]["cell_id"]["cellid"], "moment-1");
        assert_eq!(
            feeds[0]["original"]["cell_pic"]["picdata"]["pic"][0]["photourl"][0]["url"],
            "https://example.com/photo.jpg"
        );
        let replies = feeds[1]["original"]["cell_comment"]["main_comment"]["replys"]
            .as_array()
            .unwrap();
        assert_eq!(replies.len(), 2);
        assert_eq!(replies[0]["user"]["nickname"], "乙");
        assert_eq!(replies[0]["replyuser"]["nickname"], "甲");
        assert_eq!(replies[1]["user"]["nickname"], "甲");
        assert_eq!(replies[1]["replyuser"]["nickname"], "乙");
        assert_eq!(feeds[2]["comm"]["subid"], 217);
    }

    #[test]
    fn classifies_history_notifications_and_filters_decorative_media() {
        let response = r#"_Callback({html:'\x3Cli class="f-single f-s-s" data-key="fct_20001_217_3_1627745220_1_1"\x3E\x3Ca class="f-name q_namecard" link="nameCard_20001"\x3E好友甲\x3C/a\x3E\x3Ca href="//user.qzone.qq.com/10001/mood/moment-1"\x3E查看说说\x3C/a\x3E\x3Cdiv class="info-detail" data-time="1627745220"\x3E2021年7月31日 23:27\x3C/div\x3E\x3Cp class="txt-box-title ellipsis-one"\x3E本人：\t\t一条历史说说\x3C/p\x3E\x3Cimg src="//qlogo2.store.qq.com/qzone/20001/20001/50"\x3E\x3Ca class="img-item"\x3E\x3Cimg src="//a1.qpic.cn/old.jpg"\x3E\x3C/a\x3E\x3C/li\x3E\x3Cli class="f-single f-s-s" data-key="fct_30001_311_14_1627745320_1_1"\x3E\x3Ca class="f-name q_namecard" link="nameCard_30001"\x3E好友乙\x3C/a\x3E\x3Ca href="//user.qzone.qq.com/10001/mood/moment-1"\x3E查看说说\x3C/a\x3E\x3Cdiv class="info-detail" data-time="1627745320"\x3E2021年7月31日 23:28\x3C/div\x3E\x3Cp class="txt-box-title ellipsis-one"\x3E本人：一条历史说说\x3C/p\x3E\x3C/li\x3E\x3Cli class="f-single f-s-s" data-key="fct_40001_333_15_1627745420_1_1"\x3E\x3Cp class="txt-box-title ellipsis-one"\x3E本人：系统通知\x3C/p\x3E\x3C/li\x3E',opuin:'10001'});"#;
        let html = decode_history_html(response).expect("应解码旧历史消息 HTML");
        let (scanned, feeds) = history_html_as_feeds(&html, "10001", Some("本人"), 0);
        assert_eq!(scanned, 3);
        assert_eq!(feeds.len(), 2);
        assert_eq!(feeds[0]["comm"]["subid"], 217);
        assert_eq!(feeds[1]["comm"]["subid"], 2);
        assert_eq!(feeds[0]["userinfo"]["user"]["uin"], "20001");
        assert_eq!(feeds[1]["userinfo"]["user"]["uin"], "30001");
        assert_eq!(feeds[0]["original"]["cell_id"]["cellid"], "moment-1");
        assert_eq!(feeds[1]["original"]["cell_id"]["cellid"], "moment-1");
        assert_eq!(
            feeds[0]["original"]["cell_userinfo"]["user"]["uin"],
            "10001"
        );
        assert_eq!(
            feeds[0]["original"]["cell_summary"]["summary"],
            "一条历史说说"
        );
        assert_eq!(
            feeds[0]["original"]["cell_pic"]["picdata"]["pic"][0]["photourl"][0]["url"],
            "https://a1.qpic.cn/old.jpg"
        );
        assert_eq!(
            feeds[0]["original"]["cell_pic"]["picdata"]["pic"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(feeds[0]["comm"]["time"], 1_627_745_220);
        assert_eq!(feeds[0]["original"]["cell_comm"]["time"], 0);
        assert!(!feeds[0]["original"]["cell_summary"]["summary"]
            .as_str()
            .unwrap()
            .contains("\\t"));
    }

    #[test]
    fn only_skips_page_specific_response_errors() {
        assert!(!feed_error_can_skip(
            "获取空间动态失败：HTTP 500 Internal Server Error"
        ));
        assert!(feed_error_is_transient(
            "获取空间动态失败：HTTP 500 Internal Server Error"
        ));
        assert!(!feed_error_can_skip("解析空间动态失败：expected value"));
        assert!(feed_error_is_transient("解析空间动态失败：expected value"));
        assert!(!feed_error_can_skip(
            "获取空间动态失败：HTTP 429 Too Many Requests"
        ));
        assert!(feed_error_is_transient(
            "QQ 空间动态接口返回错误 -1：系统繁忙，请稍后再试"
        ));
        assert!(feed_error_can_skip(
            "QQ 空间动态接口返回错误 10001：当前游标已失效"
        ));
        assert!(!feed_error_can_skip("尚未登录 QQ 空间"));
    }
}
