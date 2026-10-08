import { forwardRef } from "react";
import type { LucideProps } from "lucide-react";

/** Shirley 助手标识。迁移时剥离 shiwen 的位图资源，改为内联矢量标记。 */
export const AiBirdIcon = forwardRef<SVGSVGElement, LucideProps>(function AiBirdIcon(
  { size = 24, ...props },
  ref,
) {
  return (
    <svg
      ref={ref}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.6}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      {...props}
    >
      <path d="M4 14c0-4 3.2-7 7-7 1.4 0 2.7.4 3.8 1.1L20 7l-1.4 3.6c.3.8.4 1.6.4 2.4 0 3.3-2.7 6-6 6H4Z" />
      <circle cx="9" cy="13" r="1" fill="currentColor" stroke="none" />
    </svg>
  );
});
