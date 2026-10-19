import type { ReactNode } from "react";
import { cn } from "@/ui/utils";

/** Pill switch matching the Settings reference: neutral track off, brand accent on. */
export function Switch({
  checked,
  onChange,
  label,
  disabled,
}: {
  checked: boolean;
  onChange: (next: boolean) => void;
  label: string;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className={cn(
        "relative inline-flex h-6 w-10 shrink-0 cursor-pointer items-center rounded-full transition-colors duration-150 ease-product",
        "focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ink",
        checked ? "bg-accent" : "bg-line-strong",
        disabled && "pointer-events-none opacity-40",
      )}
    >
      <span
        className={cn(
          "pointer-events-none block size-4 rounded-full bg-white shadow-sm transition-transform duration-150 ease-product",
          checked ? "translate-x-5" : "translate-x-1",
        )}
      />
    </button>
  );
}

/** Compact select used in a row's trailing control slot. */
export function SettingsSelect<T extends string>({
  value,
  onChange,
  options,
  label,
  disabled,
}: {
  value: T;
  onChange: (next: T) => void;
  options: { value: T; label: string }[];
  label: string;
  disabled?: boolean;
}) {
  return (
    <select
      aria-label={label}
      value={value}
      disabled={disabled}
      onChange={(event) => onChange(event.target.value as T)}
      className="h-9 cursor-pointer rounded-control border border-line bg-canvas px-2.5 text-body-sm text-ink focus:outline-2 focus:outline-offset-1 focus:outline-ink disabled:opacity-50"
    >
      {options.map((option) => (
        <option key={option.value} value={option.value}>
          {option.label}
        </option>
      ))}
    </select>
  );
}

/** One settings row: title + description on the left, a control on the right. */
export function SettingsRow({
  title,
  description,
  children,
  className,
}: {
  title: ReactNode;
  description?: ReactNode;
  children?: ReactNode;
  className?: string;
}) {
  return (
    <div className={cn("flex items-center gap-4 px-4 py-3.5", className)}>
      <div className="min-w-0 flex-1">
        <p className="text-body-sm text-ink">{title}</p>
        {description && <p className="mt-0.5 text-label leading-relaxed text-muted">{description}</p>}
      </div>
      {children && <div className="shrink-0">{children}</div>}
    </div>
  );
}

/** A titled group of rows, rendered as one bordered card (the reference layout). */
export function SettingsSection({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="mt-8">
      <h2 className="mb-2 px-1 text-label font-medium text-muted">{title}</h2>
      <div className="divide-y divide-line overflow-hidden rounded-card border border-line bg-surface">
        {children}
      </div>
    </section>
  );
}
