//! 成长计划任务自动化 —— 用 CDP 在客户端里「真实完成任务」。
//!
//! ## 定位
//!
//! `growth` 模块负责 API 侧能力（接受任务 / 领取已完成积分），
//! 但**任务的「完成」只能由客户端里的真实操作产生**（服务端按行为判定）。
//! 本模块补上这一环：驱动客户端真实输入并发送消息。
//!
//! 这不是伪造上报 —— 走的是与人工操作等价、完全合规的路径。
//!
//! ## 作用范围（重要）
//!
//! CDP 只能操作**客户端当前登录的账号**。所以：
//! - 任务清单取自「客户端当前登录账号」
//! - 要处理其它账号，先用工作台切换客户端账号，再跑本模块
//!
//! ## 自动化覆盖度
//!
//! 一部分任务本质是「发一条消息」，可以全自动；另一类是导航/设置类，
//! 需要界面交互，本模块只把它们列出来。
//!
//! ## 前置
//!
//! 客户端必须带 `--remote-debugging-port=9222` 启动。

use serde_json::{json, Value};

use crate::modules::account::load_accounts;
use crate::modules::client_cdp;
use crate::modules::config::DEFAULT_CDP_PORT;

/// 自动化动作类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// 发消息即可完成
    Chat,
    /// 需要界面操作
    Manual,
}

/// 任务 → 自动化方案。
///
/// 依据：任务的完成条件本质是「在客户端里产生某次使用行为」。
/// 对「发一条消息」类，直接把提示词发出去就能达成；
/// 对导航/设置类，需要点界面元素，目前不自动做。
pub fn task_plan(code: &str) -> (Action, Vec<&'static str>) {
    match code {
        // ---- 发消息即可 ----
        "RichMeow_Chat" => (Action::Chat, vec!["你好"]),
        "chat_5" => (
            Action::Chat,
            vec![
                "你好",
                "今天天气怎么样",
                "帮我写个短句",
                "推荐一本书",
                "讲个冷笑话",
            ],
        ),
        "skill_1" => (Action::Chat, vec!["推荐一个热门技能并演示一下"]),
        "expert_5" => (Action::Chat, vec!["召唤一个专家帮我分析问题"]),
        "Expert_team_use_3" => (Action::Chat, vec!["召唤专家团"]),
        "automation_1" => (Action::Chat, vec!["帮我创建一个每天定时执行的自动化任务"]),
        "playbook_prompt" => (Action::Chat, vec!["给我一些创作灵感"]),
        "create_canvas" => (
            Action::Chat,
            vec!["用设计创意模式帮我画一只小柴犬头像"],
        ),
        // ---- 需界面操作（列出但不自动） ----
        "Library_read"
        | "Hp_Appearance"
        | "Buddy_App"
        | "Buddy_App_QQ"
        | "Model_chat_GLM5.2"
        | "template_5"
        | "Expert_lighthouse"
        | "Expert_Philanthropy" => (Action::Manual, vec![]),
        // 系统自动完成 / 时间限定
        "first_buddy" | "black_cat" => (Action::Manual, vec![]),
        _ => (Action::Manual, vec![]),
    }
}

/// 该任务是否已无需处理（已完成/已领取）。
fn is_settled(status: &str) -> bool {
    matches!(status, "completed" | "claimed")
}

/// 取客户端当前登录账号对应的本地账号记录（用于拉取任务清单）。
fn match_local_account(uid: &str) -> Option<Value> {
    load_accounts()
        .into_iter()
        .find(|a| a.get("uid").and_then(Value::as_str) == Some(uid))
}

/// CDP 连接状态 + 当前登录账号 + 可自动化任务数（只读）。
pub async fn status() -> Value {
    let port = DEFAULT_CDP_PORT;

    let acc = match client_cdp::current_account(port) {
        Ok(v) => v,
        Err(e) => {
            return json!({
                "cdpOk": false,
                "port": port,
                "error": e,
                "hint": format!(
                    "客户端需带 --remote-debugging-port={port} 启动。\
                     若它已在运行，先完全退出再带该参数重启。"
                ),
            })
        }
    };

    if acc.get("ok").and_then(Value::as_bool) != Some(true) {
        return json!({
            "cdpOk": true,
            "port": port,
            "accountOk": false,
            "userInfo": acc,
            "hint": "已连上客户端，但读取当前账号失败。",
        });
    }

    let info = acc.get("v").cloned().unwrap_or(json!({}));
    let uid = info
        .get("uid")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let nickname = info.get("nickname").cloned().unwrap_or(Value::Null);

    let local = match_local_account(&uid);
    if local.is_none() {
        return json!({
            "cdpOk": true,
            "accountOk": false,
            "uid": uid,
            "nickname": nickname,
            "hint": "客户端当前登录的账号不在本机账号库里。请先用本工具把该账号导入/登录。",
        });
    }
    let local = local.unwrap();

    let tasks = crate::modules::growth::fetch_growth_tasks(&local).await;
    let list = crate::modules::growth::tasks_of(&tasks);
    let (auto_count, manual_count, pending_credit) = summarize(&list);

    json!({
        "cdpOk": true,
        "accountOk": true,
        "port": port,
        "uid": uid,
        "nickname": nickname,
        "accountId": local.get("id").cloned().unwrap_or(Value::Null),
        "autoCount": auto_count,
        "manualCount": manual_count,
        "pendingCredit": pending_credit,
        "pending": pending_summary(&list),
    })
}

/// 统计：可自动化数 / 需人工数 / 待完成积分。
fn summarize(tasks: &[Value]) -> (i64, i64, i64) {
    let mut auto = 0i64;
    let mut manual = 0i64;
    let mut credit = 0i64;
    for t in tasks {
        let st = t.get("accept_status").and_then(Value::as_str).unwrap_or("");
        if is_settled(st) {
            continue;
        }
        let code = t.get("task_code").and_then(Value::as_str).unwrap_or("");
        match task_plan(code).0 {
            Action::Chat => auto += 1,
            Action::Manual => manual += 1,
        }
        credit += t.get("reward_credit").and_then(Value::as_i64).unwrap_or(0);
    }
    (auto, manual, credit)
}

/// 待完成任务摘要（给前端列表用）。
fn pending_summary(tasks: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for t in tasks {
        let st = t.get("accept_status").and_then(Value::as_str).unwrap_or("");
        if is_settled(st) {
            continue;
        }
        let code = t.get("task_code").and_then(Value::as_str).unwrap_or("");
        let (action, prompts) = task_plan(code);
        out.push(json!({
            "taskCode": code,
            "title": t.get("title").cloned().unwrap_or(Value::Null),
            "credit": t.get("reward_credit").cloned().unwrap_or(json!(0)),
            "status": st,
            "auto": action == Action::Chat,
            "promptCount": prompts.len(),
            "jumpUrl": t.get("jump_url").cloned().unwrap_or(Value::Null),
        }));
    }
    out
}

/// 执行自动完成任务。
///
/// - `only`：只处理这些 task_code（None = 全部可自动的）
/// - `dry_run`：只列出将要发送什么，不实际发送
///
/// 返回每个任务的执行结果。**注意**：任务完成状态由服务端判定，
/// 发完消息不等于立刻 completed，通常需要跑一次 `growth` 领取。
pub async fn run(only: Option<Vec<String>>, dry_run: bool) -> Value {
    let port = DEFAULT_CDP_PORT;

    // 1) 确认客户端可连、且知道当前登录的是谁
    let acc = match client_cdp::current_account(port) {
        Ok(v) => v,
        Err(e) => {
            return json!({
                "ok": false,
                "stage": "connect",
                "error": e,
                "hint": format!("请让 WorkBuddy 带 --remote-debugging-port={port} 启动。"),
            })
        }
    };
    if acc.get("ok").and_then(Value::as_bool) != Some(true) {
        return json!({"ok": false, "stage": "account", "error": acc});
    }
    let info = acc.get("v").cloned().unwrap_or(json!({}));
    let uid = info.get("uid").and_then(Value::as_str).unwrap_or("").to_string();
    let Some(local) = match_local_account(&uid) else {
        return json!({
            "ok": false,
            "stage": "account",
            "error": "客户端当前登录账号不在本机账号库",
            "uid": uid,
        });
    };

    // 2) 取该账号的待完成任务
    let tasks = crate::modules::growth::fetch_growth_tasks(&local).await;
    let list = crate::modules::growth::tasks_of(&tasks);

    // 3) 挑出可自动化的
    let mut plan: Vec<(String, String, Vec<&'static str>, i64)> = Vec::new();
    for t in &list {
        let st = t.get("accept_status").and_then(Value::as_str).unwrap_or("");
        if is_settled(st) {
            continue;
        }
        let code = t.get("task_code").and_then(Value::as_str).unwrap_or("");
        if let Some(only_codes) = &only {
            if !only_codes.iter().any(|c| c == code) {
                continue;
            }
        }
        let (action, prompts) = task_plan(code);
        if action != Action::Chat || prompts.is_empty() {
            continue;
        }
        plan.push((
            code.to_string(),
            t.get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            prompts,
            t.get("reward_credit").and_then(Value::as_i64).unwrap_or(0),
        ));
    }

    if plan.is_empty() {
        return json!({
            "ok": true,
            "ran": 0,
            "message": "当前账号没有可自动完成的任务",
            "results": [],
        });
    }

    // 4) 逐个执行（CDP 是阻塞 IO，放 spawn_blocking 避免卡住 async runtime）
    let mut results = Vec::new();
    let mut sent = 0i64;
    let mut planned = 0i64;
    for (code, title, prompts, credit) in plan {
        let mut logs = Vec::new();
        let mut ok = true;

        if dry_run {
            // 演练：不发送，但要说清「将会发几条」，否则「发送 0 条」容易被误解为没生效
            planned += prompts.len() as i64;
            for p in &prompts {
                logs.push(format!("[演练] 将发送：{p}"));
            }
        } else {
            for p in prompts {
                let text = p.to_string();
                let res = tokio::task::spawn_blocking(move || {
                    client_cdp::send_message(port, &text)
                })
                .await
                .unwrap_or_else(|e| Err(format!("任务线程异常: {e}")));

                match res {
                    Ok(m) => {
                        sent += 1;
                        logs.push(format!("发送 {p:?} -> {m}"));
                    }
                    Err(e) => {
                        ok = false;
                        logs.push(format!("发送 {p:?} 失败: {e}"));
                        break;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
            }
        }

        results.push(json!({
            "taskCode": code,
            "title": title,
            "credit": credit,
            "ok": ok,
            "logs": logs,
        }));
    }

    json!({
        "ok": true,
        "dryRun": dry_run,
        "ran": results.len(),
        "sentMessages": sent,
        // 演练时 sentMessages 必然为 0，用 plannedMessages 表达「将会发几条」
        "plannedMessages": if dry_run { planned } else { sent },
        "results": results,
        "note": "任务完成状态由服务端判定。发完消息后请再执行一次「一键处理全部账号」领取积分。",
    })
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_tasks_have_prompts() {
        for code in [
            "RichMeow_Chat",
            "chat_5",
            "skill_1",
            "expert_5",
            "Expert_team_use_3",
            "automation_1",
            "playbook_prompt",
            "create_canvas",
        ] {
            let (action, prompts) = task_plan(code);
            assert_eq!(action, Action::Chat, "{code} 应为可自动");
            assert!(!prompts.is_empty(), "{code} 应有提示词");
        }
    }

    #[test]
    fn manual_tasks_are_not_auto() {
        for code in ["Library_read", "Hp_Appearance", "Buddy_App", "template_5"] {
            assert_eq!(task_plan(code).0, Action::Manual, "{code} 应为需人工");
        }
    }

    #[test]
    fn unknown_task_is_manual() {
        assert_eq!(task_plan("no_such_task").0, Action::Manual);
    }

    #[test]
    fn settled_states_are_detected() {
        assert!(is_settled("completed"));
        assert!(is_settled("claimed"));
        assert!(!is_settled("accepted"));
        assert!(!is_settled("in_progress"));
    }
}
