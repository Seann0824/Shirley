import { File, Folder, X } from "lucide-react";
import { cn } from "@/ui/utils";
import type { FileReference } from "./types";

/**
 * `@` 引用的 chip 展示。迁移自 shiwen 的 EntityChips，把实体图标收敛为
 * 文件 / 目录两种。`onRemove` 省略时为只读展示（用于已发送的用户消息回显）。
 */
export function FileChips({
  references,
  onRemove,
  className,
}: {
  references: FileReference[];
  onRemove?: (path: string) => void;
  className?: string;
}) {
  if (references.length === 0) return null;
  return (
    <div className={cn("flex flex-wrap gap-1.5", className)} data-file-chips>
      {references.map((reference) => {
        const Icon = reference.kind === "dir" ? Folder : File;
        return (
          <span
            key={reference.path}
            className="inline-flex min-w-0 max-w-full items-center gap-1 rounded-control bg-inset px-2 py-0.5 text-body-sm text-ink"
            title={reference.path}
          >
            <Icon className="size-3 shrink-0 text-muted" data-icon="inline-start" />
            <span className="min-w-0 max-w-40 truncate">{reference.name}</span>
            {onRemove && (
              <button
                type="button"
                aria-label={`移除引用：${reference.name}`}
                onClick={() => onRemove(reference.path)}
                className={cn(
                  "shrink-0 rounded-full p-0.5 text-muted transition-colors",
                  "hover:bg-selected hover:text-ink",
                )}
              >
                <X className="size-3" />
              </button>
            )}
          </span>
        );
      })}
    </div>
  );
}
