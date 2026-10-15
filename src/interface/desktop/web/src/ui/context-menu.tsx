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
 * Lightweight context menu — deliberately avoids new dependencies.
 *
 * Why build our own: sidebar actions (session pin / rename / delete, project rename /
 * archive / restore, new project) should be **hidden by default and appear on right-click**,
 * rather than each row carrying a row of hover buttons. Radix has no built-in context menu,
 * and installing `@radix-ui/react-context-menu` would add a dependency and weight; here we
 * implement a minimal usable version with `react-dom`'s `createPortal`: `<ContextMenu>`
 * wraps the trigger, and right-clicking the trigger pops up `menu` at the cursor.
 *
 * Interaction: right-click to open -> click a menu item to run and close / click blank space
 * or press Esc to close / window scroll or resize to close (avoiding the menu and anchor
 * drifting apart). When the menu would overflow the edge, it is pulled back into the viewport.
 */

type MenuState = { x: number; y: number } | null;

/** Lets a menu item close the menu after it is triggered. */
const CloseCtx = createContext<() => void>(() => {});

/** Menu item: `tone="danger"` is for destructive actions like delete. */
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
  /** Closes the menu after selection by default; set `false` for items that must stay in the menu, like double confirmation. */
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
 * Wraps the trigger: right-clicking `children` opens `menu`. `menu` may be a function
 * `(close) => ReactNode`, used when you need to close manually (e.g. double confirmation).
 */
export function ContextMenu({
  children,
  menu,
  className,
}: {
  /** A plain node = right-click only; a function = `(open) => node`, letting a custom button call `open(event)` to open the menu. */
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

  // Close: Esc / scroll / zoom (clicking blank space is handled by the overlay).
  useEffect(() => {
    if (!pos) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") close();
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("resize", close);
    // Listen for scroll in the capture phase so any ancestor scroll is caught.
    window.addEventListener("scroll", close, true);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("resize", close);
      window.removeEventListener("scroll", close, true);
    };
  }, [pos, close]);

  // Edge pull-back: measure the menu size once after render and shift up/left if it overflows the viewport.
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
