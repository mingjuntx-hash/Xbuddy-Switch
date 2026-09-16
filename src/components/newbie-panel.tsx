import { useCallback, useEffect, useState } from "react";
import { toast } from "sonner";
import { Bot, ClipboardCopy, ExternalLink, Gift, Loader2, Link2, Trophy, Wand2 } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Separator } from "@/components/ui/separator";
import { Switch } from "@/components/ui/switch";
import * as api from "@/lib/api";
import type {
  AutotaskRunResult,
  AutotaskStatus,
  NewbieConfig,
  NewbieRunAllResult,
  NewbieStatus,
} from "@/lib/types";

/**
 * 是否展示「还需在客户端完成的任务」区块。
 *
 * 已确认（2026-09-15 逆向验证）：成长计划任务的进度**完全由服务端判定**，
 * 客户端只上报性能埋点（Aegis），没有任何 task_code 上报通道
 * （`GROWTH_RPC_CHANNELS` 仅 `growth:getBuddy` 一个只读频道）；
 * 网页「去完成」也只是 `workbuddy://task?action=start` 深链，
 * 由 `TaskDeeplinkCoordinator` 把 prompt 预填进输入框**草稿态**，不自动提交。
 *
 * 因此本工具无法代替用户完成任务，只能「接受任务 + 领取已完成积分」。
 * 该区块目前隐藏，等找到可行的自动化路径再打开。
 */
const SHOW_PENDING_TASKS = false;

/**
 * 是否显示「任务自动化」区块（CDP 驱动客户端真实操作）。
 *
 * 2026-09-16 关闭：该能力依赖客户端带 `--remote-debugging-port=9222` 启动，
 * 而端口会在客户端每次重启后失效（启动参数机制使然），使用成本高于收益。
 * 改为引导用户去官方成长计划页处理。
 *
 * 实现完整保留（后端 autotask / client_cdp 模块也一并保留），
 * 改回 `true` 重新构建即可恢复。
 */
const SHOW_AUTOTASK = false;

/**
 * 任务 code → 可直接粘贴到 WorkBuddy 的提示词。
 *
 * 这些任务的进度只有「在 WorkBuddy 客户端里真实操作」才会计账，
 * 所以这里给出对应的提示词，用户复制过去逐条发送即可。
 */
const TASK_PROMPTS: Record<string, string> = {
  RichMeow_Chat: "你好",
  create_canvas: "用设计创意模式帮我画一只小柴犬头像",
  playbook_prompt: "给我一些创作灵感",
  Library_read: "打开我的资料库，找最近一份文档读一下",
  Expert_team_use_3: "召唤专家团",
  expert_5: "召唤一个专家帮我分析问题",
  skill_1: "推荐一个热门技能并演示一下",
  automation_1: "帮我创建一个每天定时执行的自动化任务",
  chat_5: "随便聊点有趣的话题",
  // 以下几项实测无法靠纯文字提示词完成，需要客户端界面操作；
  // 括号开头表示「这是操作指引，不是可直接发送的提示词」。
  Expert_lighthouse: "（客户端操作：侧边栏「专家」里找「腾讯轻量云」相关专家并召唤）",
  Expert_Philanthropy: "（客户端操作：侧边栏「专家」里找「公益」相关专家并召唤）",
  Hp_Appearance: "（客户端操作：设置 → 主题 → 和平精英）",
  Buddy_App: "（客户端操作：左侧边栏「发现应用」）",
  Buddy_App_QQ: "（客户端操作：左侧边栏「发现应用」→「企鹅教师助手」）",
  "Model_chat_GLM5.2": "（客户端操作：输入框上方模型选择器切到 GLM-5.2 并发一条消息）",
  template_5: "（客户端操作：模板中心选一个模板并使用）",
  first_buddy: "（系统自动完成，无需操作）",
  black_cat: "（需夜间时段参与活动）",
};

/** 是否是可直连发送的提示词（括号开头的是操作指引，不是提示词）。 */
function isSendablePrompt(prompt: string | undefined): boolean {
  return !!prompt && !prompt.startsWith("（");
}

/**
 * 用系统默认方式打开外部链接。
 *
 * 任务入口是 `workbuddy://home?templateId=...` 这类自定义协议，
 * opener 插件会被 capabilities 白名单拦下，所以统一走 Rust 侧的 shell 命令。
 */
async function openExternal(url: string): Promise<void> {
  await api.openExternalUrl(url);
}

/** 复制文本到剪贴板；Tauri WebView 下优先用 clipboard API，失败则降级。 */
async function copyToClipboard(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    try {
      const ta = document.createElement("textarea");
      ta.value = text;
      ta.style.position = "fixed";
      ta.style.opacity = "0";
      document.body.appendChild(ta);
      ta.select();
      const ok = document.execCommand("copy");
      document.body.removeChild(ta);
      return ok;
    } catch {
      return false;
    }
  }
}

/** 待完成任务条目（newbie_status 的 pending 项 + 所属账号）。 */
type PendingItem = NewbieStatus["pending"] extends (infer T)[] | undefined
  ? T & { accountId: string }
  : never;

/**
 * 「还需在客户端完成的任务」区块（当前默认隐藏，见 `SHOW_PENDING_TASKS`）。
 *
 * 保留完整实现是为了等找到可行的自动化路径后能一键恢复：
 * 把 `SHOW_PENDING_TASKS` 改成 true 即可。
 */
function PendingTasksBlock({ items }: { items: PendingItem[] }) {
  /** 按 taskCode 去重（同 code 的任务在不同账号上提示词相同）。 */
  const unique = (() => {
    const seen = new Set<string>();
    const out: PendingItem[] = [];
    for (const p of items) {
      if (seen.has(p.taskCode)) continue;
      seen.add(p.taskCode);
      out.push(p);
    }
    return out;
  })();

  if (unique.length === 0) return null;

  async function onCopyPrompt(taskCode: string, title: string) {
    const prompt = TASK_PROMPTS[taskCode];
    if (!prompt) {
      toast.warning("这条任务没有对应的提示词", { description: title });
      return;
    }
    if (!isSendablePrompt(prompt)) {
      toast.info("这条任务需要客户端操作", { description: prompt.slice(1, -1) });
      return;
    }
    const ok = await copyToClipboard(prompt);
    if (ok) toast.success("提示词已复制", { description: prompt });
    else toast.error("复制失败，请手动复制", { description: prompt });
  }

  async function onCopyAllPrompts() {
    const lines: string[] = [];
    for (const p of unique) {
      const prompt = TASK_PROMPTS[p.taskCode];
      if (isSendablePrompt(prompt)) lines.push(prompt);
    }
    if (lines.length === 0) {
      toast.warning("当前没有可直接发送的提示词", {
        description: "剩余任务需要到客户端界面里操作",
      });
      return;
    }
    const ok = await copyToClipboard(lines.join("\n"));
    if (ok) {
      toast.success(`已复制 ${lines.length} 条提示词`, {
        description: "切到对应账号后，在 WorkBuddy 里逐条粘贴发送即可",
      });
    } else {
      toast.error("复制失败，请手动复制");
    }
  }

  async function onOpenTask(url: string, title: string) {
    if (!url) {
      toast.info("这条任务没有跳转入口", { description: title });
      return;
    }
    try {
      await openExternal(url);
    } catch (e) {
      toast.error("打开失败", { description: api.asError(e) });
    }
  }

  return (
    <div className="rounded-lg border border-border/60 p-3">
      <div className="mb-2 flex flex-wrap items-center justify-between gap-2">
        <p className="text-xs font-medium">
          还需在客户端完成的任务（进度只有客户端真实操作才记账）
        </p>
        <Button
          size="sm"
          variant="outline"
          className="h-7 px-2 text-[11px]"
          onClick={() => void onCopyAllPrompts()}
        >
          <ClipboardCopy className="size-3.5" />
          复制全部提示词
        </Button>
      </div>
      <ul className="space-y-2 text-xs text-muted-foreground">
        {unique.slice(0, 12).map((p) => {
          const prompt = TASK_PROMPTS[p.taskCode];
          const sendable = isSendablePrompt(prompt);
          return (
            <li key={p.taskCode} className="flex items-start justify-between gap-3">
              <span className="min-w-0 flex-1">
                <span className="block truncate text-foreground/85">{p.title}</span>
                <span
                  className={
                    "block truncate text-[11px] " +
                    (sendable
                      ? "font-mono text-muted-foreground/80"
                      : "italic text-muted-foreground/65")
                  }
                >
                  {prompt ?? "（无提示词，请到客户端完成）"}
                </span>
              </span>
              <span className="flex shrink-0 items-center gap-1.5 pt-0.5">
                <span className="tabular-nums">
                  {p.current}/{p.target} · +{p.credit}
                </span>
                {p.jumpUrl && (
                  <Button
                    size="icon"
                    variant="ghost"
                    className="size-6"
                    title="打开客户端完成这条任务"
                    aria-label={`打开客户端完成：${p.title}`}
                    onClick={() => void onOpenTask(p.jumpUrl, p.title)}
                  >
                    <ExternalLink className="size-3.5" />
                  </Button>
                )}
                <Button
                  size="icon"
                  variant="ghost"
                  className="size-6"
                  title={sendable ? "复制这条提示词" : "查看操作指引"}
                  aria-label={`复制提示词：${p.title}`}
                  onClick={() => void onCopyPrompt(p.taskCode, p.title)}
                >
                  <ClipboardCopy className="size-3.5" />
                </Button>
              </span>
            </li>
          );
        })}
      </ul>
      {unique.length > 12 && (
        <p className="mt-2 text-[11px] text-muted-foreground">
          还有 {unique.length - 12} 项未显示
        </p>
      )}
      <p className="mt-2 text-[11px] leading-relaxed text-muted-foreground">
        用法：① 正体字那条是可直接发送的提示词 —— 点「复制全部提示词」，切到该账号后在
        WorkBuddy 里逐条粘贴发送；② 斜体那条需要客户端界面操作 —— 点右侧跳转图标直接打开对应入口；
        ③ 做完回来点「一键处理全部账号」领积分。
      </p>
    </div>
  );
}

/**
 * 新人礼包面板：邀请码绑定 + 新手任务自动领取。
 *
 * 两个能力：
 * 1. 新账号自动绑定邀请码（默认用预置码，也可粘贴自己的邀请链接）
 * 2. 自动接受成长计划任务并领取已完成任务的积分
 *
 * 关于「任务无法全自动完成」：任务进度只有客户端里的真实操作才会计账，
 * 所以这里能做的是「接受全部任务 + 领掉所有已完成的积分」，
 * 剩下的待完成任务会列出来并给出客户端入口，供用户去点最后一下。
 */
export function NewbiePanel({ accountCount }: { accountCount: number }) {
  const [config, setConfig] = useState<NewbieConfig | null>(null);
  const [linkInput, setLinkInput] = useState("");
  const [saving, setSaving] = useState(false);
  const [running, setRunning] = useState(false);
  const [lastRun, setLastRun] = useState<NewbieRunAllResult | null>(null);
  const [statuses, setStatuses] = useState<Record<string, NewbieStatus>>({});

  // 任务自动化（CDP）
  const [autoStatus, setAutoStatus] = useState<AutotaskStatus | null>(null);
  const [autoRunning, setAutoRunning] = useState(false);
  const [autoResult, setAutoResult] = useState<AutotaskRunResult | null>(null);

  useEffect(() => {
    let cancelled = false;
    // 首屏探测 CDP（客户端可能起得比本程序晚，失败会自动重试几轮）
    void probeWithRetry();
    void (async () => {
      try {
        const cfg = await api.getNewbieConfig();
        if (cancelled) return;
        setConfig(cfg);
        setLinkInput(cfg.invite_link || cfg.invite_code || "");
      } catch {
        /* 配置读不到时静默：面板仍可手动操作 */
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const save = useCallback(async (next: NewbieConfig) => {
    const previous = config;
    setConfig(next);
    setSaving(true);
    try {
      setConfig(await api.saveNewbieConfig(next));
      toast.success("新人礼包设置已保存");
    } catch (e) {
      setConfig(previous);
      toast.error(api.asError(e));
    } finally {
      setSaving(false);
    }
  }, [config]);

  async function onToggle(key: keyof NewbieConfig, value: boolean) {
    if (!config) return;
    await save({ ...config, [key]: value });
  }

  async function onSaveInvite() {
    if (!config) return;
    const text = linkInput.trim();
    if (!text) {
      toast.error("请填写邀请码或邀请链接");
      return;
    }
    // 交给后端解析：纯邀请码 / 完整链接都能识别
    await save({ ...config, invite_link: text, invite_code: "" });
  }

  async function onRunAll() {
    setRunning(true);
    try {
      const r = await api.runNewbieAll(true);
      setLastRun(r);
      if (r.status !== "ok") {
        toast.error(`执行未完成：${r.reason ?? r.status}`);
        return;
      }
      const claimed = r.totalClaimed ?? 0;
      const credit = r.totalCredit ?? 0;
      const secs = r.elapsedMs != null ? (r.elapsedMs / 1000).toFixed(1) : null;
      const cost = secs ? `，耗时 ${secs}s` : "";
      toast.success(
        claimed > 0
          ? `已领取 ${claimed} 个任务奖励，共 ${credit} 积分${cost}`
          : `没有可领取的奖励（任务已完成或还未完成）${cost}`,
      );
      await loadStatuses();
    } catch (e) {
      toast.error(api.asError(e));
    } finally {
      setRunning(false);
    }
  }

  async function loadStatuses() {
    // 状态接口按 accountId 查询，这里只做批量概览；失败不影响主流程
    try {
      const { accounts } = await api.getAccounts();
      const out: Record<string, NewbieStatus> = {};
      for (const a of accounts) {
        try {
          out[a.id] = await api.getNewbieStatus(a.id);
        } catch {
          /* 单账号失败跳过 */
        }
      }
      setStatuses(out);
    } catch {
      /* 忽略 */
    }
  }

  /** 读 CDP 连接状态 + 客户端当前账号 + 待办统计。 */
  async function loadAutotaskStatus() {
    try {
      setAutoStatus(await api.autotaskStatus());
    } catch (e) {
      setAutoStatus({ cdpOk: false, error: api.asError(e) });
    }
  }

  /**
   * 首屏探测：客户端可能比本程序启动得晚，所以失败时重试几次。
   *
   * 每次间隔 2.5s，最多 4 次 —— 覆盖「先开本程序、后开客户端」的场景。
   */
  async function probeWithRetry(attempts = 4) {
    for (let i = 0; i < attempts; i++) {
      try {
        const s = await api.autotaskStatus();
        setAutoStatus(s);
        if (s.cdpOk) return; // 连上了就停
      } catch (e) {
        setAutoStatus({ cdpOk: false, error: api.asError(e) });
      }
      if (i < attempts - 1) {
        await new Promise((r) => setTimeout(r, 2500));
      }
    }
  }

  /** 诊断客户端 DOM（排查用）。 */
  async function onAutotaskProbe() {
    setAutoRunning(true);
    try {
      const r = await api.autotaskProbe();
      const dom = (r as { dom?: { chosen?: unknown } })?.dom;
      if ((r as { ok?: boolean })?.ok) {
        toast.success("诊断完成", {
          description: dom?.chosen
            ? `已找到输入框：${JSON.stringify(dom.chosen)}`
            : "已连上客户端，但没找到可见输入框（检查是否停在首页/对话页）",
        });
      } else {
        toast.error("诊断失败", {
          description: String((r as { error?: string })?.error ?? "未知错误"),
        });
      }
    } catch (e) {
      toast.error("诊断失败", { description: api.asError(e) });
    } finally {
      setAutoRunning(false);
      void loadAutotaskStatus();
    }
  }

  /** 执行自动完成任务。dryRun=true 时只列出将要发送什么。 */
  async function onAutotaskRun(dryRun: boolean) {
    setAutoRunning(true);
    try {
      const r = await api.autotaskRun(undefined, dryRun);
      setAutoResult(r);
      if (!r.ok) {
        toast.error("未能执行", {
          description: String(r.hint ?? r.message ?? r.stage ?? "未知原因"),
        });
        return;
      }
      if ((r.ran ?? 0) === 0) {
        toast.info(r.message ?? "当前账号没有可自动完成的任务");
        return;
      }
      toast.success(
        dryRun
          ? `演练：${r.ran} 个任务，将发送 ${r.plannedMessages ?? 0} 条消息（未实际发送）`
          : `已发送 ${r.sentMessages ?? 0} 条消息，覆盖 ${r.ran} 个任务`,
        {
          description: dryRun
            ? "这是彩排，什么都没发。确认无误后点「自动完成任务」才真正执行。"
            : "任务状态由服务端判定，稍等片刻再点「一键处理全部账号」领取积分",
        },
      );
      await loadAutotaskStatus();
    } catch (e) {
      toast.error(api.asError(e));
    } finally {
      setAutoRunning(false);
    }
  }

  const pendingItems: PendingItem[] = Object.entries(statuses).flatMap(([id, s]) =>
    (s.pending ?? []).map((p) => ({ ...p, accountId: id })),
  );

  return (
    <Card className="mt-6">
      <CardHeader className="pb-3">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div className="flex items-center gap-2">
            <Gift className="size-4 text-muted-foreground" />
            <CardTitle className="text-base">新人礼包</CardTitle>
            <Badge variant="secondary" className="h-6 rounded-full border-0 px-2 text-[11px]">
              邀请码 + 新手任务
            </Badge>
          </div>
          <div className="flex items-center gap-2.5">
            <Label htmlFor="newbie-enabled" className="cursor-pointer text-xs text-muted-foreground">
              启用
            </Label>
            <Switch
              id="newbie-enabled"
              checked={config?.enabled ?? false}
              disabled={!config || saving}
              onCheckedChange={(v) => void onToggle("enabled", v)}
            />
            {saving && <Loader2 className="size-3.5 animate-spin text-muted-foreground" />}
          </div>
        </div>
        <CardDescription className="text-xs leading-relaxed">
          新账号自动绑定邀请码领取新人积分；同时自动接受「成长计划」任务并领掉已完成任务的积分。
        </CardDescription>
      </CardHeader>

      <CardContent className="space-y-4">
        {/* 邀请码 */}
        <div className="space-y-2">
          <Label htmlFor="newbie-invite" className="flex items-center gap-1.5 text-xs">
            <Link2 className="size-3.5" />
            邀请码 / 邀请链接
          </Label>
          <div className="flex gap-2">
            <Input
              id="newbie-invite"
              placeholder="粘贴邀请码，或 https://www.workbuddy.cn/events/invite/?inviteCode=xxxx"
              value={linkInput}
              disabled={!config || saving}
              onChange={(e) => setLinkInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void onSaveInvite();
              }}
            />
            <Button
              variant="secondary"
              disabled={!config || saving}
              onClick={() => void onSaveInvite()}
            >
              保存
            </Button>
          </div>
          {config?.invite_code && (
            <p className="text-[11px] text-muted-foreground">
              当前生效邀请码：<span className="font-mono">{config.invite_code}</span>
            </p>
          )}
        </div>

        <Separator />

        {/* 开关组 */}
        <div className="grid gap-3 sm:grid-cols-3">
          <label className="flex items-center gap-2.5 text-xs">
            <Switch
              checked={config?.auto_bind_on_add ?? false}
              disabled={!config || saving}
              onCheckedChange={(v) => void onToggle("auto_bind_on_add", v)}
            />
            添加账号后自动绑定
          </label>
          <label className="flex items-center gap-2.5 text-xs">
            <Switch
              checked={config?.auto_accept_tasks ?? false}
              disabled={!config || saving}
              onCheckedChange={(v) => void onToggle("auto_accept_tasks", v)}
            />
            自动接受任务
          </label>
          <label className="flex items-center gap-2.5 text-xs">
            <Switch
              checked={config?.auto_claim_tasks ?? false}
              disabled={!config || saving}
              onCheckedChange={(v) => void onToggle("auto_claim_tasks", v)}
            />
            自动领取积分
          </label>
        </div>

        <Separator />

        {/* 执行 */}
        <div className="flex flex-wrap items-center gap-2">
          <Button
            disabled={running || accountCount === 0}
            onClick={() => void onRunAll()}
          >
            {running ? (
              <Loader2 className="size-4 animate-spin" />
            ) : (
              <Wand2 className="size-4" />
            )}
            一键处理全部账号
          </Button>
        </div>

        {lastRun?.accounts && lastRun.accounts.length > 0 && (
          <div className="rounded-lg border border-border/60 p-3">
            <p className="mb-2 text-xs font-medium">上次执行结果</p>
            <ul className="space-y-1 text-xs text-muted-foreground">
              {lastRun.accounts.map((a) => (
                <li key={a.accountId ?? a.account} className="flex items-center justify-between gap-3">
                  <span className="truncate">{a.account}</span>
                  <span className="shrink-0 tabular-nums">
                    领取 {a.claimedCount ?? 0} 个 / {a.claimedCredit ?? 0} 分
                  </span>
                </li>
              ))}
            </ul>
          </div>
        )}

        {/* 任务自动化（CDP 驱动客户端真实操作）—— 由 SHOW_AUTOTASK 控制显示 */}
        {SHOW_AUTOTASK && (
        <div className="space-y-3 rounded-lg border border-border/60 p-3">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div className="flex items-center gap-2">
              <Bot className="size-3.5 text-muted-foreground" />
              <p className="text-xs font-medium">任务自动化</p>
            </div>
            <Badge
              variant={autoStatus?.cdpOk ? "default" : "secondary"}
              className="h-5 rounded-full border-0 px-2 text-[10px]"
            >
              {autoStatus === null
                ? "未检测"
                : autoStatus.cdpOk
                  ? `客户端已连接${autoStatus.nickname ? ` · ${autoStatus.nickname}` : ""}`
                  : "客户端未连接"}
            </Badge>
          </div>

          <p className="text-[11px] leading-relaxed text-muted-foreground">
            任务的「完成」只有客户端里的真实操作才记账。这里通过调试端口（
            <span className="font-mono">--remote-debugging-port=9222</span>
            ）驱动客户端<b>真实输入并发送消息</b>，等价于人工操作。
            <br />
            注意：只能操作<b>客户端当前登录的账号</b>。
          </p>

          {autoStatus && !autoStatus.cdpOk && (
            <div className="space-y-1 rounded-md bg-muted/50 p-2 text-[11px] leading-relaxed text-muted-foreground">
              <p>
                {autoStatus.hint ?? "请让 WorkBuddy 带调试端口启动。"}
              </p>
              {autoStatus.error && (
                <p className="font-mono break-all opacity-70">{autoStatus.error}</p>
              )}
            </div>
          )}

          {autoStatus?.cdpOk && (
            <div className="flex flex-wrap gap-3 text-[11px] text-muted-foreground">
              <span>
                可自动 <b className="text-foreground">{autoStatus.autoCount ?? 0}</b> 个
              </span>
              <span>
                需人工 <b className="text-foreground">{autoStatus.manualCount ?? 0}</b> 个
              </span>
              <span>
                待完成约 <b className="text-foreground">{autoStatus.pendingCredit ?? 0}</b> 分
              </span>
            </div>
          )}

          {autoStatus?.cdpOk && (autoStatus.pending?.length ?? 0) > 0 && (
            <ul className="space-y-1 text-[11px]">
              {autoStatus.pending!.slice(0, 10).map((p) => (
                <li key={p.taskCode} className="flex items-center justify-between gap-2">
                  <span className="truncate text-foreground/85">{p.title ?? p.taskCode}</span>
                  <span className="shrink-0">
                    <span className={p.auto ? "text-emerald-600 dark:text-emerald-400" : "text-muted-foreground"}>
                      {p.auto ? "可自动" : "需人工"}
                    </span>
                    <span className="ml-2 tabular-nums text-muted-foreground">+{p.credit ?? 0}</span>
                  </span>
                </li>
              ))}
            </ul>
          )}

          <div className="flex flex-wrap items-center gap-2">
            <Button
              size="sm"
              variant="outline"
              disabled={autoRunning}
              onClick={() => void loadAutotaskStatus()}
            >
              检测连接
            </Button>
            <Button
              size="sm"
              variant="outline"
              disabled={autoRunning || !autoStatus?.cdpOk}
              onClick={() => void onAutotaskProbe()}
            >
              诊断界面
            </Button>
            <Button
              size="sm"
              variant="outline"
              disabled={autoRunning || !autoStatus?.cdpOk}
              onClick={() => void onAutotaskRun(true)}
            >
              演练
            </Button>
            <Button
              size="sm"
              disabled={autoRunning || !autoStatus?.cdpOk}
              onClick={() => void onAutotaskRun(false)}
            >
              {autoRunning ? (
                <Loader2 className="size-3.5 animate-spin" />
              ) : (
                <Bot className="size-3.5" />
              )}
              自动完成任务
            </Button>
          </div>

          {autoResult?.results && autoResult.results.length > 0 && (
            <div className="rounded-md border border-border/50 p-2">
              <p className="mb-1 text-[11px] font-medium">执行明细</p>
              <ul className="space-y-1 text-[11px] text-muted-foreground">
                {autoResult.results.map((r) => (
                  <li key={r.taskCode}>
                    <span className={r.ok ? "text-emerald-600 dark:text-emerald-400" : "text-destructive"}>
                      {r.ok ? "✓" : "✗"}
                    </span>{" "}
                    <span className="text-foreground/85">{r.title ?? r.taskCode}</span>
                    <span className="ml-1 tabular-nums">+{r.credit ?? 0}</span>
                    {r.logs && r.logs.length > 0 && (
                      <span className="ml-1">（{r.logs.length} 条）</span>
                    )}
                  </li>
                ))}
              </ul>
            </div>
          )}
        </div>
        )}

        {/* 成长计划入口：去官方页面完成任务，回来领积分 */}
        <div className="flex flex-wrap items-center gap-2 rounded-lg border border-border/60 p-3">
          <Button
            size="sm"
            variant="outline"
            onClick={() =>
              void api.openExternalUrl(
                "https://www.workbuddy.cn/profile/growth-center",
              )
            }
          >
            <Trophy className="size-3.5" />
            成长计划 · 完成任务领积分
          </Button>
          <span className="text-[11px] leading-relaxed text-muted-foreground">
            在官方页面完成任务后，回到这里点「一键处理全部账号」领取积分
          </span>
        </div>

        {SHOW_PENDING_TASKS && <PendingTasksBlock items={pendingItems} />}
      </CardContent>
    </Card>
  );
}
