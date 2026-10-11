import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import { cn } from "./utils";

/**
 * 轻量右键菜单（context menu）——刻意不引入新依赖。
 *
 * 为什么自建：左侧栏的操作（会话置顶 / 重命名 / 删除，项目重命名 / 归档 / 恢复、
 * 新建项目）希望**默认隐藏、右键才出现**，而不是每个条目挂一排悬停按钮。Radix 没有
 * 内置 context-menu，装 `@radix-ui/react-context-menu` 会新增依赖与体积；这里用
 * `react-dom` 的 `createPortal` 实现最小可用版：`<ContextMenu>` 包住触发器，在触发器
 * 上右键即在光标处弹出 `menu`。
 *
 * 交互：右键打开 → 点菜单项执行并关闭 / 点空白或 Esc 关闭 / 窗口滚动或 resize 关闭
 * （避免菜单与锚点错位）。菜单贴边时自动回收到视口内。
 */

type MenuState = { x: number; y: number } | null;

/** 供菜单项在触发后关闭菜单。 */
const CloseCtx = createContext<() => void>(() => {});

/** 菜单项：`tone="danger"` 用于删除这类破坏性操作。 */
export function ContextMenuItem({
  onSelect,
  children,
  icon,
  tone = "default",
  disabled = false,
  closeOnSelect = true,
}: {
  onSelect: () => void;
  children: ReactNode;
  icon?: ReactNode;
  tone?: "default" | "danger";
  disabled?: boolean;
  /** 默认点选后关闭菜单；二次确认这类需要留在菜单内的项设 `false`。 */
  closeOnSelect?: boolean;
}) {
  const close = useContext(CloseCtx);
  return (
    <button
      type="button"
      role="menuitem"
      disabled={disabled}
      className={cn(
        "flex w-full items-center gap-2 rounded-control px-2 py-1.5 text-left text-body-sm transition-colors disabled:pointer-events-none disabled:opacity-45",
        tone === "danger" ? "text-danger-ink hover:bg-danger-soft" : "text-ink hover:bg-quiet",
      )}
      onClick={(event) => {
        event.stopPropagation();
        onSelect();
        if (closeOnSelect) close();
      }}
    >
      {icon && (
        <span className="flex size-4 shrink-0 items-center justify-center text-muted">{icon}</span>
      )}
      <span className="min-w-0 flex-1 truncate">{children}</span>
    </button>
  );
}

export function ContextMenuSeparator() {
  return <div className="my-1 h-px bg-line" role="separator" />;
}

/**
 * 包住触发器：在 `children` 上右键即打开 `menu`。`menu` 可传函数
 * `(close) => ReactNode`，需要手动关闭（如二次确认）时用。
 */
export function ContextMenu({
  children,
  menu,
  className,
}: {
  /** 普通节点 = 仅右键触发；函数 = `(open) => node`，可让自定义按钮调用 `open(event)` 打开菜单。 */
  children: ReactNode | ((open: (event: React.MouseEvent) => void) => ReactNode);
  menu: ReactNode | ((close: () => void) => ReactNode);
  className?: string;
}) {
  const [pos, setPos] = useState<MenuState>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const [adjusted, setAdjusted] = useState({ x: 0, y: 0 });

  const open = useCallback((event: React.MouseEvent) => {
    event.preventDefault();
    event.stopPropagation();
    setPos({ x: event.clientX, y: event.clientY });
    setAdjusted({ x: event.clientX, y: event.clientY });
  }, []);

  const close = useCallback(() => setPos(null), []);

  // 关闭：Esc / 滚动 / 缩放（点空白由遮罩层处理）。
  useEffect(() => {
    if (!pos) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") close();
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("resize", close);
    // capture 阶段监听滚动，任何祖先滚动都能收到。
    window.addEventListener("scroll", close, true);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("resize", close);
      window.removeEventListener("scroll", close, true);
    };
  }, [pos, close]);

  // 贴边回收：渲染后量一次菜单尺寸，超出视口就往上/往左挪。
  useLayoutEffect(() => {
    if (!pos || !menuRef.current) return;
    const rect = menuRef.current.getBoundingClientRect();
    const margin = 8;
    let x = pos.x;
    let y = pos.y;
    if (x + rect.width > window.innerWidth - margin) x = window.innerWidth - margin - rect.width;
    if (y + rect.height > window.innerHeight - margin) y = window.innerHeight - margin - rect.height;
    setAdjusted({ x: Math.max(margin, x), y: Math.max(margin, y) });
  }, [pos]);

  return (
    <>
      <div className={className} onContextMenu={open}>
        {typeof children === "function" ? children(open) : children}
      </div>
      {pos &&
        createPortal(
          <div
            className="fixed inset-0 z-50"
            onContextMenu={(event) => {
              event.preventDefault();
              close();
            }}
            onMouseDown={(event) => {
              if (event.button === 0) close();
            }}
          >
            <div
              ref={menuRef}
              role="menu"
              className="absolute min-w-44 rounded-card border border-line bg-overlay p-1 shadow-overlay"
              style={{ left: adjusted.x, top: adjusted.y }}
              onMouseDown={(event) => event.stopPropagation()}
              onContextMenu={(event) => event.preventDefault()}
            >
              <CloseCtx.Provider value={close}>
                {typeof menu === "function" ? menu(close) : menu}
              </CloseCtx.Provider>
            </div>
          </div>,
          document.body,
        )}
    </>
  );
}
