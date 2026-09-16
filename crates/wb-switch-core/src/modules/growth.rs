//! 成长计划自动化：新人邀请码绑定 + 新手任务自动接受/领取。
//!
//! 这一模块补上 xbuddy-switch 缺的两块能力：
//!
//! 1. **新账号自动绑定邀请码** —— 添加账号后自动调
//!    `POST /activity/workbuddy/invitation/v2/bind`，让新号吃到新人礼包。
//! 2. **新手任务自动接受 + 领取** —— 调成长计划任务接口，把
//!    `not_accepted` 的任务一次性接受，再把 `completed` 的积分领掉。
//!
//! ## 关于「任务无法全自动完成」
//!
//! 任务的**进度**（`progress.current`）只有客户端里的真实操作才会记账：
//! 客户端主进程把活动事件（`desktopHost:monitorReportPromptDone` 等）直报服务端，
//! 网页端点「去完成」只做「接受 + `workbuddy://` 跳转」两件事。
//! 所以本模块能做到的是：**接受全部任务 + 领取所有已完成任务的积分**，
//! 以及把待完成任务的入口整理出来（`newbie_status` 返回 `pending` 列表），
//! 由调用方引导用户去客户端点最后一下。
//!
//! ## 状态机
//!
//! `not_accepted` -> `accepted` -> `in_progress` -> `completed` -> `claimed`
//!
//! 领取只对 `completed` 有效；`claimed` 重复领取会返回业务错误，属正常。

use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;

use crate::modules::account::{account_display_name, build_auth_headers, load_accounts};
use crate::modules::config::{
    add_newbie_log, http_request, load_newbie_config, now_ms, parse_invite_code, RunFlagGuard,
    GROWTH_API_PREFIX, INVITE_API_PREFIX, WORKBUDDY_API_ENDPOINT,
};
use crate::modules::refresh::refresh_account_token;

static NEWBIE_RUNNING: AtomicBool = AtomicBool::new(false);

/// 任务状态常量（与服务端 `accept_status` 取值一致）。
pub const ST_NOT_ACCEPTED: &str = "not_accepted";
pub const ST_ACCEPTED: &str = "accepted";
pub const ST_IN_PROGRESS: &str = "in_progress";
pub const ST_COMPLETED: &str = "completed";
pub const ST_CLAIMED: &str = "claimed";

/// 邀请绑定返回码 -> 中文说明（对照网页端错误码表）。
pub fn invite_code_message(code: i64) -> &'static str {
    match code {
        0 => "绑定成功",
        12310 => "该账号已绑定过邀请码，无需重复绑定",
        12311 => "仅活动期间新注册的用户可绑定邀请码（老账号无法绑定）",
        12312 => "活动已结束，无法绑定邀请码",
        12313 => "已静默处理",
        12314 => "该邀请码不适用于当前活动",
        12315 => "收货地址已提交锁定，无法修改",
        12316 => "绑定请求被拒绝，请联系客服",
        12319 => "邀请码不存在，请检查后重试",
        _ => "未知返回码",
    }
}

fn account_key(account: &Value) -> String {
    account
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(String::from)
        .unwrap_or_else(|| account_display_name(account))
}

/// 成长/邀请接口的请求头。
///
/// 邀请接口挂在 `/activity/...` 下（非 `/v2/plugin` 体系），需要额外带上
/// `x-client-platform` / `origin` / `referer`，否则可能被网关拒绝。
fn build_growth_headers(account: &Value) -> HashMap<String, String> {
    let mut headers = build_auth_headers(account);
    headers.insert("x-client-platform".to_string(), "web".to_string());
    headers.insert("origin".to_string(), WORKBUDDY_API_ENDPOINT.to_string());
    headers.insert(
        "referer".to_string(),
        format!("{WORKBUDDY_API_ENDPOINT}/profile/growth-center"),
    );
    headers
}

fn is_unauthorized(resp: &Value) -> bool {
    let code = resp.get("code").and_then(Value::as_i64).unwrap_or(-1);
    if code == 401 || code == 403 {
        return true;
    }
    let msg = resp
        .get("message")
        .or_else(|| resp.get("msg"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();
    ["unauthorized", "401", "登录", "失效", "过期", "token"]
        .iter()
        .any(|k| msg.contains(k))
}

/// 发成长接口请求；未授权且有 refresh_token 时刷新一次并重试。
async fn growth_request(path: &str, method: &str, body: Option<Value>, account: &Value) -> Value {
    let url = format!("{WORKBUDDY_API_ENDPOINT}{path}");
    let headers = build_growth_headers(account);
    let mut resp = http_request(&url, method, body.clone(), Some(&headers)).await;
    if is_unauthorized(&resp)
        && !account
            .get("refresh_token")
            .and_then(Value::as_str)
            .unwrap_or("")
            .is_empty()
    {
        let refreshed = refresh_account_token(account.clone()).await;
        let headers = build_growth_headers(&refreshed);
        resp = http_request(&url, method, body, Some(&headers)).await;
    }
    resp
}

fn resp_msg(resp: &Value) -> String {
    resp.get("message")
        .or_else(|| resp.get("msg"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn resp_ok(resp: &Value) -> bool {
    resp.get("code").and_then(Value::as_i64) == Some(0)
}

// ---------------------------------------------------------------------------
// 邀请码绑定
// ---------------------------------------------------------------------------

/// 为一个账号绑定邀请码。
///
/// 返回 `{ ok, code, message, alreadyBound }`。
/// `ok` 为 true 的情况包括「绑定成功」和「已经绑定过」—— 两者都不需要再重试。
pub async fn bind_invite_code(account: &Value, invite_code: &str) -> Value {
    let code = parse_invite_code(invite_code);
    if code.is_empty() {
        return json!({
            "ok": false,
            "code": -1,
            "message": "邀请码为空或格式无法识别",
            "alreadyBound": false,
        });
    }

    let path = format!("{INVITE_API_PREFIX}/bind");
    let resp = growth_request(&path, "POST", Some(json!({ "inviteCode": code })), account).await;

    let biz = resp.get("code").and_then(Value::as_i64).unwrap_or(-1);
    let message = resp_msg(&resp);
    let already = biz == 12310;

    json!({
        "ok": biz == 0 || already,
        "code": biz,
        "message": if message.is_empty() {
            invite_code_message(biz).to_string()
        } else {
            message
        },
        "alreadyBound": already,
    })
}

/// 读取当前账号的邀请进度（我的邀请码、已邀请人数、累计积分）。
pub async fn fetch_invite_progress(account: &Value) -> Value {
    let progress = growth_request(
        &format!("{INVITE_API_PREFIX}/my-progress"),
        "GET",
        None,
        account,
    )
    .await;

    // my-progress 不含邀请码本身，单独取一次
    let mine = growth_request(
        &format!("{INVITE_API_PREFIX}/my-code"),
        "GET",
        None,
        account,
    )
    .await;

    let d = progress.get("data").cloned().unwrap_or(json!({}));
    let md = mine.get("data").cloned().unwrap_or(json!({}));

    json!({
        "ok": resp_ok(&progress),
        "inviteCode": md.get("invite_code").and_then(Value::as_str).unwrap_or(""),
        "inviteCount": d.get("invite_count").and_then(Value::as_i64).unwrap_or(0),
        "validInviteCount": d.get("valid_invite_count").and_then(Value::as_i64).unwrap_or(0),
        "totalCredits": d.get("total_credits").and_then(Value::as_i64).unwrap_or(0),
        "invitedUsers": d.get("invited_users").cloned().unwrap_or(json!([])),
        "raw": progress,
    })
}

// ---------------------------------------------------------------------------
// 成长计划任务
// ---------------------------------------------------------------------------

/// 拉取成长计划任务列表（原始响应）。
pub async fn fetch_growth_tasks(account: &Value) -> Value {
    growth_request(
        &format!("{GROWTH_API_PREFIX}/tasks"),
        "GET",
        None,
        account,
    )
    .await
}

/// 拉取成长等级信息。
pub async fn fetch_growth_profile(account: &Value) -> Value {
    growth_request(
        &format!("{GROWTH_API_PREFIX}/profile"),
        "GET",
        None,
        account,
    )
    .await
}

/// 从任务响应里取出 tasks 数组。
pub fn tasks_of(resp: &Value) -> Vec<Value> {
    resp.get("data")
        .and_then(|d| d.get("tasks"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// 批量接受任务。
pub async fn accept_growth_tasks(account: &Value, codes: &[String]) -> Value {
    if codes.is_empty() {
        return json!({"ok": true, "accepted": 0, "message": "没有待接受的任务"});
    }
    let path = format!("{GROWTH_API_PREFIX}/tasks/accept");
    let resp = growth_request(
        &path,
        "POST",
        Some(json!({ "task_codes": codes })),
        account,
    )
    .await;
    json!({
        "ok": resp_ok(&resp),
        "accepted": if resp_ok(&resp) { codes.len() } else { 0 },
        "message": resp_msg(&resp),
        "raw": resp,
    })
}

/// 领取单个任务的积分。
pub async fn claim_growth_task(account: &Value, task_code: &str) -> Value {
    let path = format!("{GROWTH_API_PREFIX}/tasks/{task_code}/claim");
    let resp = growth_request(&path, "POST", None, account).await;
    json!({
        "ok": resp_ok(&resp),
        "code": resp.get("code").and_then(Value::as_i64).unwrap_or(-1),
        "message": resp_msg(&resp),
        "raw": resp,
    })
}

/// 汇总任务状态：可领取积分、待完成积分、待完成任务清单。
pub fn summarize_tasks(tasks: &[Value]) -> Value {
    let mut earnable: i64 = 0;
    let mut pending_credit: i64 = 0;
    let mut counts: Map<String, Value> = Map::new();
    let mut pending: Vec<Value> = Vec::new();

    for t in tasks {
        let st = t
            .get("accept_status")
            .and_then(Value::as_str)
            .unwrap_or(ST_NOT_ACCEPTED);
        let credit = t.get("reward_credit").and_then(Value::as_i64).unwrap_or(0);
        let cur = t
            .get("progress")
            .and_then(|p| p.get("current"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let tgt = t
            .get("progress")
            .and_then(|p| p.get("target"))
            .and_then(Value::as_i64)
            .unwrap_or(0);

        let bucket = counts.entry(st.to_string()).or_insert(json!(0));
        if let Some(n) = bucket.as_i64() {
            *bucket = json!(n + 1);
        }

        match st {
            ST_COMPLETED => earnable += credit,
            ST_NOT_ACCEPTED | ST_ACCEPTED | ST_IN_PROGRESS => {
                if tgt == 0 || cur < tgt {
                    pending_credit += credit;
                    pending.push(json!({
                        "taskCode": t.get("task_code").and_then(Value::as_str).unwrap_or(""),
                        "title": t.get("title").and_then(Value::as_str).unwrap_or(""),
                        "credit": credit,
                        "acceptStatus": st,
                        "current": cur,
                        "target": tgt,
                        "jumpUrl": t.get("jump_url").and_then(Value::as_str).unwrap_or(""),
                        "description": t.get("description").and_then(Value::as_str).unwrap_or(""),
                    }));
                }
            }
            _ => {}
        }
    }

    json!({
        "total": tasks.len(),
        "earnableCredit": earnable,
        "pendingCredit": pending_credit,
        "statusCounts": Value::Object(counts),
        "pending": pending,
    })
}

// ---------------------------------------------------------------------------
// 编排：单个账号 / 全部账号
// ---------------------------------------------------------------------------

/// 对一个账号执行新人流程：绑定邀请码 -> 接受任务 -> 领取积分。
///
/// `bind_invite` 为 false 时跳过邀请码绑定（用于「只补领任务」场景）。
pub async fn run_newbie_for_account(account: &Value, bind_invite: bool) -> Value {
    let cfg = load_newbie_config();
    let invite_code = cfg
        .get("invite_code")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let auto_accept = cfg
        .get("auto_accept_tasks")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let auto_claim = cfg
        .get("auto_claim_tasks")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    let label = account_display_name(account);
    let mut steps: Vec<Value> = Vec::new();

    // ---- 1) 绑定邀请码 ----
    if bind_invite {
        let r = bind_invite_code(account, &invite_code).await;
        steps.push(json!({
            "step": "bind-invite",
            "ok": r.get("ok").and_then(Value::as_bool).unwrap_or(false),
            "code": r.get("code"),
            "message": r.get("message"),
            "inviteCode": invite_code,
        }));
    }

    // ---- 2) 拉任务 ----
    let tasks_resp = fetch_growth_tasks(account).await;
    if !resp_ok(&tasks_resp) {
        steps.push(json!({
            "step": "fetch-tasks",
            "ok": false,
            "message": resp_msg(&tasks_resp),
        }));
        let result = json!({
            "ok": false,
            "account": label,
            "steps": steps,
        });
        log_run(account, &result);
        return result;
    }

    let mut tasks = tasks_of(&tasks_resp);

    // ---- 3) 接受未接受的任务 ----
    if auto_accept {
        let to_accept: Vec<String> = tasks
            .iter()
            .filter(|t| {
                t.get("accept_status").and_then(Value::as_str) == Some(ST_NOT_ACCEPTED)
            })
            .filter_map(|t| {
                t.get("task_code")
                    .and_then(Value::as_str)
                    .filter(|c| !c.is_empty())
                    .map(String::from)
            })
            .collect();

        if !to_accept.is_empty() {
            let r = accept_growth_tasks(account, &to_accept).await;
            steps.push(json!({
                "step": "accept-tasks",
                "ok": r.get("ok"),
                "accepted": r.get("accepted"),
                "taskCodes": to_accept,
                "message": r.get("message"),
            }));
            // 接受后重新拉一次，才能看到变为 completed 的项
            tokio::time::sleep(std::time::Duration::from_millis(600)).await;
            let refreshed = fetch_growth_tasks(account).await;
            if resp_ok(&refreshed) {
                tasks = tasks_of(&refreshed);
            }
        } else {
            steps.push(json!({
                "step": "accept-tasks",
                "ok": true,
                "accepted": 0,
                "message": "没有待接受的任务",
            }));
        }
    }

    // ---- 4) 领取已完成任务的积分 ----
    let mut claimed_points: i64 = 0;
    let mut claimed_count = 0;
    let mut claimed_titles: Vec<String> = Vec::new();

    if auto_claim {
        // 先把已完成且未领取的挑出来
        let claimable: Vec<(String, String, i64)> = tasks
            .iter()
            .filter(|t| t.get("accept_status").and_then(Value::as_str) == Some(ST_COMPLETED))
            .filter_map(|t| {
                let code = t.get("task_code").and_then(Value::as_str)?.to_string();
                let title = t
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let credit = t.get("reward_credit").and_then(Value::as_i64).unwrap_or(0);
                Some((code, title, credit))
            })
            .collect();

        for (code, title, credit) in &claimable {
            let r = claim_growth_task(account, code).await;
            if r.get("ok").and_then(Value::as_bool) == Some(true) {
                claimed_count += 1;
                claimed_points += credit;
                claimed_titles.push(title.clone());
            }
        }

        steps.push(json!({
            "step": "claim-tasks",
            "ok": true,
            "claimed": claimed_count,
            "credit": claimed_points,
            "titles": claimed_titles,
        }));
    }

    // ---- 5) 汇总 ----
    let summary = summarize_tasks(&tasks);
    let result = json!({
        "ok": true,
        "account": label,
        "accountId": account_key(account),
        "claimedCount": claimed_count,
        "claimedCredit": claimed_points,
        "earnableCredit": summary.get("earnableCredit"),
        "pendingCredit": summary.get("pendingCredit"),
        "pending": summary.get("pending"),
        "statusCounts": summary.get("statusCounts"),
        "steps": steps,
    });
    log_run(account, &result);
    result
}

fn log_run(account: &Value, result: &Value) {
    add_newbie_log(&json!({
        "ts": now_ms(),
        "accountId": account_key(account),
        "email": account_display_name(account),
        "ok": result.get("ok"),
        "claimedCount": result.get("claimedCount"),
        "claimedCredit": result.get("claimedCredit"),
        "pendingCredit": result.get("pendingCredit"),
    }));
}

/// 对所有账号跑一遍新人流程（只领取，不重复绑定）。
///
/// 并发执行：每个账号的流程是 3~4 轮独立 HTTP 往返（各自带自己的 token），
/// 串行时总耗时随账号数线性增长。这里用信号量限流并发（默认 4），
/// 既把总耗时压到接近单账号耗时，又不会瞬间打出几十个请求。
///
/// 注意：`add_newbie_log` 是读-改-写，已在 config.rs 里加了锁；
/// 结果按原账号顺序回填，保证前端展示顺序稳定。
pub async fn run_newbie_all(bind_invite: bool) -> Value {
    let Some(_guard) = RunFlagGuard::try_acquire(&NEWBIE_RUNNING) else {
        return json!({"status": "skipped", "reason": "already_running"});
    };
    let cfg = load_newbie_config();
    if cfg.get("enabled").and_then(Value::as_bool) != Some(true) {
        return json!({"status": "disabled"});
    }
    let accounts = load_accounts();
    if accounts.is_empty() {
        return json!({"status": "no_accounts"});
    }

    /// 同时处理的账号数上限（避免瞬间打出过多请求）。
    const MAX_CONCURRENCY: usize = 4;

    let started = std::time::Instant::now();
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENCY));
    let mut set = tokio::task::JoinSet::new();

    for (idx, acc) in accounts.iter().enumerate() {
        let acc = acc.clone();
        let sem = std::sync::Arc::clone(&sem);
        set.spawn(async move {
            let Ok(_permit) = sem.acquire().await else {
                return (idx, json!({"ok": false, "account": "?", "reason": "semaphore_closed"}));
            };
            let r = run_newbie_for_account(&acc, bind_invite).await;
            (idx, r)
        });
    }

    // 按原顺序回填，保证展示顺序与账号列表一致
    let mut ordered: Vec<Option<Value>> = vec![None; accounts.len()];
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((idx, r)) => ordered[idx] = Some(r),
            Err(e) => {
                // 任务 panic：记录但不影响其它账号
                let _ = e;
            }
        }
    }

    let mut results = Vec::new();
    let mut total_claimed: i64 = 0;
    let mut total_credit: i64 = 0;
    for r in ordered.into_iter().flatten() {
        total_claimed += r.get("claimedCount").and_then(Value::as_i64).unwrap_or(0);
        total_credit += r.get("claimedCredit").and_then(Value::as_i64).unwrap_or(0);
        results.push(r);
    }

    json!({
        "status": "ok",
        "accounts": results,
        "totalClaimed": total_claimed,
        "totalCredit": total_credit,
        "elapsedMs": started.elapsed().as_millis() as u64,
        "concurrency": MAX_CONCURRENCY,
    })
}

/// 只读：单个账号的新人礼包状态（配置 + 邀请进度 + 任务概览）。
pub async fn newbie_status(account: &Value) -> Value {
    let cfg = load_newbie_config();
    let invite = fetch_invite_progress(account).await;
    let tasks_resp = fetch_growth_tasks(account).await;
    let tasks = tasks_of(&tasks_resp);
    let summary = summarize_tasks(&tasks);
    let profile = fetch_growth_profile(account).await;
    let pdata = profile.get("data").cloned().unwrap_or(json!({}));

    json!({
        "ok": resp_ok(&tasks_resp),
        "config": cfg,
        "inviteProgress": invite,
        "level": pdata.get("level"),
        "levelName": pdata.get("level_name"),
        "completedTasks": pdata.get("completed"),
        "totalTasks": pdata.get("total"),
        "earnableCredit": summary.get("earnableCredit"),
        "pendingCredit": summary.get("pendingCredit"),
        "statusCounts": summary.get("statusCounts"),
        "pending": summary.get("pending"),
    })
}
