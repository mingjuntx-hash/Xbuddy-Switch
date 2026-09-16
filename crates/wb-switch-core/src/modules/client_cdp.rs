//! 最小 CDP（Chrome DevTools Protocol）客户端 —— 用于驱动 WorkBuddy 桌面客户端。
//!
//! ## 为什么需要它
//!
//! 成长计划任务的进度由**服务端按客户端真实行为**判定。API 侧只能
//! 「接受任务 + 领取已完成积分」，无法把任务「做掉」。而 WorkBuddy 是
//! Electron 应用，自带 `--remote-debugging-port=9222`，可以用 CDP 在渲染
//! 进程里**真实输入并发送**，产生与人工操作等价的行为。
//!
//! 本模块只做「输入」这件事 —— 不伪造任何上报，走的是真实用户路径。
//!
//! ## 为什么手写 WebSocket 而不用 tungstenite
//!
//! CDP 只需要 text frame，握手所需的 `sha1` / `base64` 项目里都已有依赖，
//! 手写约 150 行即可，避免为单文件 exe 引入一整棵新依赖树。
//!
//! ## 前置
//!
//! 客户端必须带 `--remote-debugging-port=9222` 启动，否则端口不通。
//!
//! ## 安全
//!
//! 只调用 `Runtime.evaluate` 与 `Input.*` 这类无副作用的观察/输入接口，
//! 绝不调用可能改变应用状态的 IPC channel（参见 wb_guard 的黑名单教训）。

use base64::Engine as _;
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

/// 默认调试端口。
pub const DEFAULT_PORT: u16 = 9222;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const IO_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// HTTP 探测（手写，避免额外依赖）
// ---------------------------------------------------------------------------

/// 向 CDP 的 HTTP 端点发一个 GET，返回响应体。
///
/// 注意：**不能依赖连接关闭来判定响应结束**。CDP 的 HTTP 端点即使收到
/// `Connection: close` 也未必主动断开，用 read_to_end 会一直阻塞到超时
/// （实测报 WSAETIMEDOUT 10060）。所以这里按 `Content-Length` 精确读取。
fn http_get(port: u16, path: &str) -> Result<String, String> {
    let mut stream = TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}")
            .parse()
            .map_err(|e| format!("地址解析失败: {e}"))?,
        CONNECT_TIMEOUT,
    )
    .map_err(|e| format!("连接 127.0.0.1:{port} 失败（客户端是否带 --remote-debugging-port={port} 启动？）: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(8))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(8))).ok();

    let req = format!(
        "GET {path} HTTP/1.1\r\n\
         Host: 127.0.0.1:{port}\r\n\
         Accept: application/json\r\n\
         Connection: close\r\n\r\n"
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("发送请求失败: {e}"))?;

    let mut raw: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 8192];

    // 1) 读到响应头结束
    let header_end = loop {
        if let Some(i) = find_subslice(&raw, b"\r\n\r\n") {
            break i + 4;
        }
        let n = stream
            .read(&mut tmp)
            .map_err(|e| format!("读取响应头失败: {e}"))?;
        if n == 0 {
            return Err("连接被关闭（未收到完整响应头）".to_string());
        }
        raw.extend_from_slice(&tmp[..n]);
    };

    let head = String::from_utf8_lossy(&raw[..header_end]).to_string();

    // 2) 按 Content-Length 精确读 body
    let clen: Option<usize> = head
        .lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok());

    if let Some(len) = clen {
        while raw.len() < header_end + len {
            let n = stream
                .read(&mut tmp)
                .map_err(|e| format!("读取响应体失败（已收 {} 字节，期望 {len}）: {e}", raw.len()))?;
            if n == 0 {
                break;
            }
            raw.extend_from_slice(&tmp[..n]);
        }
        let end = (header_end + len).min(raw.len());
        return Ok(String::from_utf8_lossy(&raw[header_end..end]).to_string());
    }

    // 3) 没有 Content-Length：尽力读到 EOF，超时也接受已收数据
    loop {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => raw.extend_from_slice(&tmp[..n]),
            Err(_) => break,
        }
    }
    Ok(String::from_utf8_lossy(&raw[header_end..]).to_string())
}

/// 列出所有可调试 target。
pub fn list_targets(port: u16) -> Result<Vec<Value>, String> {
    let body = http_get(port, "/json/list")?;
    serde_json::from_str::<Vec<Value>>(&body).map_err(|e| {
        format!(
            "解析 /json/list 失败: {e}；原始响应前 200 字符: {}",
            body.chars().take(200).collect::<String>()
        )
    })
}

/// 找到 WorkBuddy 主渲染页的 `webSocketDebuggerUrl`。
pub fn find_main_page(port: u16) -> Result<(String, Value), String> {
    let targets = list_targets(port)?;
    let pick = targets
        .iter()
        .find(|t| {
            t.get("type").and_then(Value::as_str) == Some("page")
                && t.get("url")
                    .and_then(Value::as_str)
                    .map(|u| u.contains("renderer/index.html"))
                    .unwrap_or(false)
        })
        .or_else(|| {
            targets
                .iter()
                .find(|t| t.get("type").and_then(Value::as_str) == Some("page"))
        })
        .ok_or_else(|| "没找到可用的 page target（客户端主窗口未就绪？）".to_string())?;

    let ws = pick
        .get("webSocketDebuggerUrl")
        .and_then(Value::as_str)
        .ok_or_else(|| "target 缺少 webSocketDebuggerUrl".to_string())?
        .to_string();
    Ok((ws, pick.clone()))
}

// ---------------------------------------------------------------------------
// 极简 WebSocket
// ---------------------------------------------------------------------------

struct Ws {
    stream: TcpStream,
    buf: Vec<u8>,
}

impl Ws {
    fn connect(ws_url: &str) -> Result<Self, String> {
        let rest = ws_url
            .strip_prefix("ws://")
            .ok_or_else(|| format!("不支持的 WebSocket 地址: {ws_url}"))?;
        let (hostport, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let (host, port) = match hostport.find(':') {
            Some(i) => (
                &hostport[..i],
                hostport[i + 1..].parse::<u16>().unwrap_or(80),
            ),
            None => (hostport, 80),
        };

        let addr = format!("{host}:{port}");
        let stream = TcpStream::connect_timeout(
            &addr.parse().map_err(|e| format!("地址解析失败: {e}"))?,
            CONNECT_TIMEOUT,
        )
        .map_err(|e| format!("连接 CDP WebSocket 失败: {e}"))?;
        stream.set_read_timeout(Some(IO_TIMEOUT)).ok();
        stream.set_write_timeout(Some(IO_TIMEOUT)).ok();

        // 随机 16 字节 key（uuid v4 的 128 位足够）
        let key_bytes = uuid::Uuid::new_v4().into_bytes();
        let key = base64::engine::general_purpose::STANDARD.encode(key_bytes);

        // RFC6455：Accept = base64(sha1(key + 固定 GUID))
        let mut hasher = Sha1::new();
        hasher.update(key.as_bytes());
        hasher.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
        let expected_accept =
            base64::engine::general_purpose::STANDARD.encode(hasher.finalize());

        let req = format!(
            "GET {path} HTTP/1.1\r\n\
             Host: {host}:{port}\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Key: {key}\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n"
        );

        let mut ws = Ws {
            stream,
            buf: Vec::new(),
        };
        ws.stream
            .write_all(req.as_bytes())
            .map_err(|e| format!("发送握手请求失败: {e}"))?;

        // 读握手响应
        loop {
            if let Some(i) = find_subslice(&ws.buf, b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&ws.buf[..i]).to_string();
                ws.buf.drain(..i + 4);
                let status = head.lines().next().unwrap_or("");
                if !status.contains("101") {
                    return Err(format!("WebSocket 握手失败: {status}"));
                }
                // 校验 Accept，确认对方确实是 WebSocket 服务
                let accept_ok = head
                    .lines()
                    .filter_map(|l| l.split_once(':'))
                    .any(|(k, v)| {
                        k.eq_ignore_ascii_case("sec-websocket-accept")
                            && v.trim() == expected_accept
                    });
                if !accept_ok {
                    return Err("WebSocket 握手校验失败（Sec-WebSocket-Accept 不匹配）".to_string());
                }
                break;
            }
            let mut chunk = [0u8; 4096];
            let n = ws
                .stream
                .read(&mut chunk)
                .map_err(|e| format!("读握手响应失败: {e}"))?;
            if n == 0 {
                return Err("握手过程中连接被关闭".to_string());
            }
            ws.buf.extend_from_slice(&chunk[..n]);
        }
        Ok(ws)
    }

    fn send_text(&mut self, text: &str) -> Result<(), String> {
        let data = text.as_bytes();
        let mut frame = Vec::with_capacity(data.len() + 14);
        frame.push(0x81); // FIN + opcode=text
        let n = data.len();
        if n < 126 {
            frame.push(0x80 | n as u8);
        } else if n < 65536 {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(n as u16).to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(n as u64).to_be_bytes());
        }
        // 客户端发出的帧必须掩码
        let mask = uuid::Uuid::new_v4().into_bytes();
        frame.extend_from_slice(&mask[..4]);
        for (i, b) in data.iter().enumerate() {
            frame.push(b ^ mask[i % 4]);
        }
        self.stream
            .write_all(&frame)
            .map_err(|e| format!("发送帧失败: {e}"))
    }

    fn read_exact(&mut self, n: usize) -> Result<Vec<u8>, String> {
        while self.buf.len() < n {
            let mut chunk = [0u8; 65536];
            let got = self
                .stream
                .read(&mut chunk)
                .map_err(|e| format!("读取帧失败: {e}"))?;
            if got == 0 {
                return Err("连接已关闭".to_string());
            }
            self.buf.extend_from_slice(&chunk[..got]);
        }
        let out = self.buf[..n].to_vec();
        self.buf.drain(..n);
        Ok(out)
    }

    /// 读一个 text 帧（跳过 ping/pong/其它）。
    fn recv_text(&mut self, deadline: Instant) -> Result<String, String> {
        loop {
            if Instant::now() > deadline {
                return Err("等待 CDP 响应超时".to_string());
            }
            let head = self.read_exact(2)?;
            let opcode = head[0] & 0x0F;
            let masked = head[1] & 0x80 != 0;
            let mut len = (head[1] & 0x7F) as u64;
            if len == 126 {
                let b = self.read_exact(2)?;
                len = u16::from_be_bytes([b[0], b[1]]) as u64;
            } else if len == 127 {
                let b = self.read_exact(8)?;
                len = u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
            }
            let mask = if masked { Some(self.read_exact(4)?) } else { None };
            let mut payload = if len > 0 {
                self.read_exact(len as usize)?
            } else {
                Vec::new()
            };
            if let Some(m) = mask {
                for (i, b) in payload.iter_mut().enumerate() {
                    *b ^= m[i % 4];
                }
            }
            match opcode {
                0x1 => return Ok(String::from_utf8_lossy(&payload).to_string()),
                0x8 => return Err("服务端关闭了 WebSocket".to_string()),
                _ => continue, // ping / pong / binary：忽略
            }
        }
    }
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

// ---------------------------------------------------------------------------
// CDP 会话
// ---------------------------------------------------------------------------

/// 一个 target 上的 CDP 会话。
pub struct Cdp {
    ws: Ws,
    next_id: u64,
}

impl Cdp {
    pub fn connect_main_page(port: u16) -> Result<(Self, Value), String> {
        let (ws_url, target) = find_main_page(port)?;
        let ws = Ws::connect(&ws_url)?;
        Ok((
            Cdp {
                ws,
                next_id: 0,
            },
            target,
        ))
    }

    pub fn call(&mut self, method: &str, params: Option<Value>) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        let mut msg = json!({ "id": id, "method": method });
        if let Some(p) = params {
            msg["params"] = p;
        }
        self.ws.send_text(&msg.to_string())?;

        let deadline = Instant::now() + IO_TIMEOUT;
        loop {
            let raw = self.ws.recv_text(deadline)?;
            let obj: Value = match serde_json::from_str(&raw) {
                Ok(v) => v,
                Err(_) => continue, // 非 JSON 或事件帧，跳过
            };
            if obj.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(err) = obj.get("error") {
                    return Err(format!("CDP {method} 返回错误: {err}"));
                }
                return Ok(obj.get("result").cloned().unwrap_or(Value::Null));
            }
            // 其它 id 的响应/事件：忽略
        }
    }

    /// 执行 JS 表达式，返回其值（按值返回）。
    pub fn evaluate(&mut self, expression: &str) -> Result<Value, String> {
        let res = self.call(
            "Runtime.evaluate",
            Some(json!({
                "expression": expression,
                "awaitPromise": true,
                "returnByValue": true,
                "userGesture": true,
            })),
        )?;
        if let Some(ex) = res.get("exceptionDetails") {
            let desc = ex
                .get("exception")
                .and_then(|e| e.get("description"))
                .and_then(Value::as_str)
                .or_else(|| ex.get("text").and_then(Value::as_str))
                .unwrap_or("未知 JS 异常");
            return Err(format!("JS 异常: {desc}"));
        }
        Ok(res
            .get("result")
            .and_then(|r| r.get("value"))
            .cloned()
            .unwrap_or(Value::Null))
    }

    /// 把 JSON 结果用 `JSON.stringify` 包一层再取回，便于解析成结构。
    pub fn eval_json(&mut self, expr: &str) -> Result<Value, String> {
        let wrapped = format!(
            "(function(){{try{{return JSON.stringify({expr})}}catch(e){{return JSON.stringify({{__err:String(e&&e.message)}})}}}})()"
        );
        let raw = self.evaluate(&wrapped)?;
        let s = raw.as_str().unwrap_or("null");
        serde_json::from_str::<Value>(s).map_err(|e| format!("解析 JS 结果失败: {e}"))
    }
}

// ---------------------------------------------------------------------------
// 高层能力：诊断 / 发消息
// ---------------------------------------------------------------------------

/// 诊断脚本：列出输入框候选与可见按钮，帮助定位选择器。
const PROBE_JS: &str = r#"
(function(){
  var out = {candidates: [], chosen: null, buttons: []};
  var sels = ['[contenteditable="true"]','div[role="textbox"]','textarea'];
  for (var i=0;i<sels.length;i++){
    var els = Array.from(document.querySelectorAll(sels[i]));
    for (var j=0;j<els.length;j++){
      var e = els[j], r = e.getBoundingClientRect();
      var vis = e.offsetParent !== null && r.width > 40 && r.height > 10;
      out.candidates.push({
        sel: sels[i], tag: e.tagName,
        cls: String(e.className||'').slice(0,80),
        ph: e.getAttribute('data-placeholder')||e.getAttribute('placeholder')||'',
        w: Math.round(r.width), h: Math.round(r.height), vis: vis
      });
      if (vis && !out.chosen) out.chosen = {sel: sels[i], h: Math.round(r.height)};
    }
  }
  out.buttons = Array.from(document.querySelectorAll('button'))
    .filter(function(b){var r=b.getBoundingClientRect();return b.offsetParent!==null&&r.width>0&&r.height>0})
    .map(function(b){return {
      aria: b.getAttribute('aria-label')||'', title: b.getAttribute('title')||'',
      txt: (b.innerText||'').trim().slice(0,16),
      cls: String(b.className||'').slice(0,60)};})
    .slice(0, 50);
  return out;
})()
"#;

/// 连接状态 + 诊断信息。
pub fn probe(port: u16) -> Value {
    match Cdp::connect_main_page(port) {
        Ok((mut cdp, target)) => {
            let probe_val = cdp.eval_json(PROBE_JS).unwrap_or(Value::Null);
            let title = cdp
                .evaluate("document.title")
                .unwrap_or(Value::Null);
            let url = cdp
                .evaluate("location.href")
                .unwrap_or(Value::Null);
            json!({
                "ok": true,
                "port": port,
                "targetTitle": target.get("title").cloned().unwrap_or(Value::Null),
                "targetUrl": target.get("url").cloned().unwrap_or(Value::Null),
                "documentTitle": title,
                "href": url,
                "dom": probe_val,
            })
        }
        Err(e) => json!({
            "ok": false,
            "port": port,
            "error": e,
            "hint": format!("请让 WorkBuddy 带 --remote-debugging-port={port} 启动；\
若客户端已在运行，需要先完全退出再带参数重启。"),
        }),
    }
}

/// 聚焦输入框并注入文本（真实输入事件，React 可感知）。
fn focus_and_type(cdp: &mut Cdp, text: &str) -> Result<String, String> {
    let focus_res = cdp.evaluate(
        r#"(function(){
  var sels = ['[contenteditable="true"]','div[role="textbox"]','textarea'];
  for (var i=0;i<sels.length;i++){
    var els = Array.from(document.querySelectorAll(sels[i]))
      .filter(function(e){var r=e.getBoundingClientRect();
        return e.offsetParent!==null && r.height>10 && r.width>40;});
    if (els.length){ var el = els[els.length-1]; el.focus();
      return 'FOCUSED:' + el.tagName; }
  }
  return 'NO_INPUT';
})()"#,
    )?;
    let fr = focus_res.as_str().unwrap_or("");
    if !fr.starts_with("FOCUSED") {
        return Err(format!("找不到可聚焦的输入框（{fr}）"));
    }

    // 清空已有草稿，避免拼到旧内容后面
    cdp.evaluate(
        r#"(function(){
  var el = document.activeElement;
  if(!el) return 'NO_ACTIVE';
  if (el.tagName === 'TEXTAREA' || el.tagName === 'INPUT') { el.value=''; }
  else { el.innerHTML=''; }
  return 'CLEARED';
})()"#,
    )?;

    // 真实文本注入
    cdp.call("Input.insertText", Some(json!({ "text": text })))?;

    let got = cdp
        .evaluate(
            r#"(function(){
  var el = document.activeElement;
  if(!el) return '';
  return (el.textContent||el.value||'').slice(0,60);
})()"#,
        )
        .unwrap_or(Value::Null);
    Ok(format!("TYPED({})", got.as_str().unwrap_or("")))
}

/// 按真实 Enter 键（发送）。
fn press_enter(cdp: &mut Cdp) -> Result<(), String> {
    for t in ["rawKeyDown", "keyUp"] {
        cdp.call(
            "Input.dispatchKeyEvent",
            Some(json!({
                "type": t,
                "windowsVirtualKeyCode": 13,
                "nativeVirtualKeyCode": 13,
                "key": "Enter",
                "code": "Enter",
                "text": "\r",
                "unmodifiedText": "\r",
            })),
        )?;
    }
    Ok(())
}

/// 在客户端里发一条消息（最小验证 / 任务自动化基本动作）。
pub fn send_message(port: u16, text: &str) -> Result<String, String> {
    let (mut cdp, _t) = Cdp::connect_main_page(port)?;
    let typed = focus_and_type(&mut cdp, text)?;
    std::thread::sleep(Duration::from_millis(300));
    press_enter(&mut cdp)?;
    Ok(format!("OK {typed}"))
}

/// 读客户端当前登录的账号信息（只读调用，走 `auth:getAccount`）。
///
/// 用途：任务自动化只能作用于「客户端当前登录的账号」，
/// 需要先知道它是谁，才能取到该账号的任务清单。
///
/// 只调用 `auth:getAccount` 这一个明确只读的 channel —— 绝不试探其它 channel
/// （历史上误调 `auth:logout` / `window:close` 造成过状态变更）。
pub fn current_account(port: u16) -> Result<Value, String> {
    let (mut cdp, _t) = Cdp::connect_main_page(port)?;
    let raw = cdp.evaluate(
        "(async()=>{try{const r=await window.__wbInvoke('auth:getAccount');\
         return JSON.stringify({ok:true,v:r})}catch(e){\
         return JSON.stringify({ok:false,m:String(e&&e.message)})}})()",
    )?;
    let s = raw.as_str().unwrap_or("null");
    let obj: Value =
        serde_json::from_str(s).map_err(|e| format!("解析账号信息失败: {e}；原始: {s}"))?;
    Ok(obj)
}

// ---------------------------------------------------------------------------
// 实测探针（默认忽略，手动跑）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod live_tests {
    use super::*;

    /// 逐层探测与本机真实客户端的连通性，定位失败环节。
    ///
    /// 跑法（客户端需带调试端口）：
    /// ```text
    /// cargo test -p wb-switch-core --release probe_live -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    fn probe_live() {
        let port = DEFAULT_PORT;
        println!("\n=== 1) HTTP /json/list ===");
        match list_targets(port) {
            Ok(ts) => {
                println!("OK  targets = {}", ts.len());
                for t in &ts {
                    println!(
                        "    [{}] {}",
                        t.get("type").and_then(Value::as_str).unwrap_or("?"),
                        t.get("url").and_then(Value::as_str).unwrap_or("")
                    );
                }
            }
            Err(e) => println!("FAIL {e}"),
        }

        println!("\n=== 2) find_main_page ===");
        match find_main_page(port) {
            Ok((ws, t)) => println!(
                "OK  ws = {ws}\n    title = {}",
                t.get("title").and_then(Value::as_str).unwrap_or("")
            ),
            Err(e) => println!("FAIL {e}"),
        }

        println!("\n=== 3) WebSocket 握手 + Runtime.evaluate ===");
        match Cdp::connect_main_page(port) {
            Ok((mut cdp, _)) => {
                match cdp.evaluate("1+1") {
                    Ok(v) => println!("OK  1+1 = {v}"),
                    Err(e) => println!("FAIL evaluate: {e}"),
                }
                match cdp.evaluate("document.title") {
                    Ok(v) => println!("OK  title = {v}"),
                    Err(e) => println!("FAIL title: {e}"),
                }
            }
            Err(e) => println!("FAIL connect: {e}"),
        }

        println!("\n=== 4) current_account ===");
        match current_account(port) {
            Ok(v) => println!("OK  {v}"),
            Err(e) => println!("FAIL {e}"),
        }

        println!("\n=== 5) probe(port) ===");
        let p = probe(port);
        println!("{}", serde_json::to_string_pretty(&p).unwrap_or_default());

        // 输入管线逐步计时：定位「发消息」卡在哪一步
        println!("\n=== 6) 输入管线逐步计时 ===");
        match Cdp::connect_main_page(port) {
            Ok((mut cdp, _)) => {
                let t = Instant::now();
                let focus = cdp.evaluate(
                    r#"(function(){
  var sels = ['[contenteditable="true"]','div[role="textbox"]','textarea'];
  for (var i=0;i<sels.length;i++){
    var els = Array.from(document.querySelectorAll(sels[i]))
      .filter(function(e){var r=e.getBoundingClientRect();
        return e.offsetParent!==null && r.height>10 && r.width>40;});
    if (els.length){ var el = els[els.length-1]; el.focus();
      return 'FOCUSED:' + el.tagName; }
  }
  return 'NO_INPUT';
})()"#,
                );
                println!("  a) focus      {:?}  ({:?})", focus, t.elapsed());

                let t = Instant::now();
                let clear = cdp.evaluate(
                    r#"(function(){
  var el = document.activeElement;
  if(!el) return 'NO_ACTIVE';
  if (el.tagName === 'TEXTAREA' || el.tagName === 'INPUT') { el.value=''; }
  else { el.innerHTML=''; }
  return 'CLEARED:' + el.tagName;
})()"#,
                );
                println!("  b) clear      {:?}  ({:?})", clear, t.elapsed());

                let t = Instant::now();
                let ins = cdp.call(
                    "Input.insertText",
                    Some(json!({ "text": "任务自动化连通性测试" })),
                );
                println!("  c) insertText {:?}  ({:?})", ins.map(|_| "ok"), t.elapsed());

                let t = Instant::now();
                let got = cdp.evaluate(
                    r#"(function(){
  var el = document.activeElement;
  if(!el) return '';
  return (el.textContent||el.value||'').slice(0,60);
})()"#,
                );
                println!("  d) 读回内容   {:?}  ({:?})", got, t.elapsed());

                let t = Instant::now();
                let ent = press_enter(&mut cdp);
                println!("  e) Enter      {:?}  ({:?})", ent.map(|_| "ok"), t.elapsed());
            }
            Err(e) => println!("FAIL connect: {e}"),
        }
    }
}
