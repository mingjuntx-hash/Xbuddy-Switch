import { useEffect, useState } from "react";
import { Loader2 } from "lucide-react";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import * as api from "@/lib/api";
import type { AccountMeta } from "@/lib/types";

/**
 * 备注长度上限（XBuddy 分支自有功能）。
 *
 * 备注最终是账号卡片头部的一枚小 chip（落在邮箱那一行下面），太长只会被截断、
 * 反而看不出关键信息。60 个字符够写清「这个号主要干嘛」（例如「主力号 · 公司项目 · 10/12 到期」），
 * 也能在紧凑模式最窄的卡片（300px）里占一行放得下。
 *
 * 上限同时在前端（`maxLength`，输入时就拦住）与后端（`account::set_note` 不做长度校验，
 * 由本常量把关）两侧表达：这里拦不住粘贴以外的一切，所以后端不设上限是刻意的——
 * 老数据或别的端写进来的更长备注不会被本机截断或拒绝。
 */
export const NOTE_MAX_LENGTH = 60;

interface Props {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** 目标账号（null = 关闭） */
  account: AccountMeta | null;
  /** 保存成功后回调：调用方据此同步账号列表里的备注 */
  onSaved?: () => void;
}

/** 账号名（用于提示文案与 toast）：昵称 > 邮箱 > uid > id。 */
function accountLabel(account: AccountMeta): string {
  return account.nickname || account.email || account.uid || account.id;
}

/**
 * 账号备注编辑弹窗。
 *
 * 一个账号一句备注，写在**本机账号库**里（`~/.wb-switch/accounts.json` 的 `note` 键），
 * 目的是切号时一眼分辨「这个号是干嘛的」。因为账号库在 Rust 侧是透传的 `Value`，
 * 该键与上游 / 其它端共存不冲突。
 *
 * 留空保存 = 清除备注（后端会删掉该键，不留 `"note": ""`）。
 */
export function AccountNoteDialog({ open, onOpenChange, account, onSaved }: Props) {
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  // 每次打开都以账号当前备注为初值；关闭时丢弃草稿（重开回到已保存的内容，不会留下半截输入）。
  useEffect(() => {
    if (!open) return;
    setValue(account?.note ?? "");
    setBusy(false);
    setError("");
  }, [open, account]);

  /** 当前已保存的备注（后端写入时已做首尾裁剪，这里同样裁剪后再比较，避免「只多个空格」也发一次请求）。 */
  const saved = account?.note ?? "";
  const next = value.trim();
  const unchanged = next === saved;

  async function save() {
    if (!account || busy) return;
    // 没有任何改动就直接关掉：不发请求、也不弹提示。
    if (unchanged) {
      onOpenChange(false);
      return;
    }
    setBusy(true);
    setError("");
    try {
      await api.setAccountNote(account.id, next);
      toast.success(next ? "备注已保存" : "备注已清除", {
        description: next
          ? `「${accountLabel(account)}」：${next}`
          : `「${accountLabel(account)}」的备注已清除`,
      });
      onOpenChange(false);
      onSaved?.();
    } catch (e) {
      setError(api.asError(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={(o) => (busy ? undefined : onOpenChange(o))}>
      <DialogContent className="sm:max-w-md">
        <form
          className="min-w-0 space-y-4"
          onSubmit={(e) => {
            e.preventDefault();
            void save();
          }}
        >
          <DialogHeader className="min-w-0">
            <DialogTitle>账号备注</DialogTitle>
            <DialogDescription className="min-w-0 break-words">
              {account ? `${accountLabel(account)} · ` : ""}
              备注只写在本机账号库，用来分辨这个账号主要做什么。
            </DialogDescription>
          </DialogHeader>

          <div className="min-w-0 space-y-1.5">
            <Input
              // Radix 默认聚焦内容区里第一个可聚焦元素，这里就是本输入框；
              // 显式 autoFocus 让「打开即改」在键盘操作下也成立。
              autoFocus
              value={value}
              maxLength={NOTE_MAX_LENGTH}
              placeholder="例如：主力号 · 公司项目"
              aria-label="账号备注"
              aria-invalid={error ? true : undefined}
              disabled={busy}
              onChange={(e) => setValue(e.target.value)}
            />
            <div className="flex items-center justify-between gap-3 text-xs text-muted-foreground">
              <span className="min-w-0 truncate">留空保存即可清除备注</span>
              <span className="shrink-0 tabular-nums">
                {value.length}/{NOTE_MAX_LENGTH}
              </span>
            </div>
            {error && <div className="break-all text-xs text-destructive">{error}</div>}
          </div>

          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              disabled={busy}
              onClick={() => onOpenChange(false)}
            >
              取消
            </Button>
            <Button type="submit" disabled={busy}>
              {busy ? (
                <>
                  <Loader2 className="animate-spin" />
                  保存中…
                </>
              ) : (
                "保存"
              )}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
