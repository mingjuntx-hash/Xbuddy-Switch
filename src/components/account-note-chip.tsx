import { StickyNote } from "lucide-react";

import { cn } from "@/lib/utils";
import type { AccountMeta } from "@/lib/types";

/**
 * 取已保存的备注（XBuddy 分支自有字段）。
 *
 * 后端写入时已裁剪首尾空白，这里再兜一次：老数据或别的端写进来的空串按「没写」处理，
 * 免得渲染出一个只有图标、一个字都没有的空 chip。
 */
export function accountNote(account: AccountMeta | null | undefined): string {
  return account?.note?.trim() ?? "";
}

/**
 * 账号备注 chip：解决「一堆账号分不清哪个是干嘛的」。
 *
 * 用琥珀色而不是中性灰，是刻意的：备注是**用户自己写的信息**，和旁边的状态 chip
 * （系统推导出来的签到 / 旅行 / 限额）不是一类东西，颜色必须能区分开，
 * 否则会被当成「又一个状态标签」，那就白写了。
 *
 * - 卡片里传 `editable` 时渲染成按钮，点一下直接开编辑；
 * - 演示模式或纯展示位置（切号弹窗标题下）退化为 `<span>`，只读不改。
 *
 * 宽度规则：`w-fit max-w-full` + 内层 `truncate` —— 卡片窄时截断而不是把标题挤变形，
 * 完整文案交给 `title`（原生 tooltip）兜底。
 */
export function AccountNoteChip({
  note,
  editable,
  onEdit,
  className,
}: {
  note: string;
  /** 可点击编辑（需要同时给 `onEdit`）。 */
  editable?: boolean;
  onEdit?: () => void;
  className?: string;
}) {
  const base = cn(
    "flex w-fit max-w-full min-w-0 items-center gap-1 rounded-md bg-amber-500/10 px-1.5 py-0.5 text-left text-[11px] font-medium leading-4 text-amber-700 dark:text-amber-400",
    editable &&
      "cursor-pointer transition-colors hover:bg-amber-500/20 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/40",
    className,
  );
  const body = (
    <>
      <StickyNote className="size-3 shrink-0" aria-hidden="true" />
      <span className="min-w-0 truncate">{note}</span>
    </>
  );
  if (!editable) {
    return (
      <span className={base} title={`备注：${note}`}>
        {body}
      </span>
    );
  }
  return (
    <button
      type="button"
      className={base}
      // 备注本身已可见，title 只在被截断时补全；尾注点明可点击，避免「看着像纯展示」。
      title={`备注：${note}（点击编辑）`}
      aria-label={`编辑备注：${note}`}
      onClick={onEdit}
    >
      {body}
    </button>
  );
}
