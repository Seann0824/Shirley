import * as React from "react";
import { cva, type VariantProps } from "class-variance-authority";
import { Slot } from "@radix-ui/react-slot";
import { cn } from "./utils";

const badgeVariants = cva(
  "inline-flex w-fit shrink-0 items-center justify-center gap-1 overflow-hidden rounded-full border border-transparent px-2 py-0.5 font-utility text-caption whitespace-nowrap transition-[color,box-shadow] focus-visible:border-ink/40 focus-visible:ring-2 focus-visible:ring-ink/30 aria-invalid:border-danger aria-invalid:ring-danger/20 [&>svg]:pointer-events-none [&>svg]:size-3",
  {
    variants: {
      variant: {
        default: "bg-primary text-primary-foreground [a&]:hover:bg-primary/85",
        secondary: "bg-selected text-ink [a&]:hover:bg-inset",
        destructive: "bg-danger text-primary-foreground [a&]:hover:bg-danger/85",
        outline: "border-line text-ink [a&]:hover:bg-quiet",
        ghost: "[a&]:hover:bg-quiet [a&]:hover:text-ink",
        link: "text-ink underline-offset-4 [a&]:hover:underline",
      },
    },
    defaultVariants: { variant: "default" },
  },
);

function Badge({
  className,
  variant = "default",
  asChild = false,
  ...props
}: React.ComponentProps<"span"> & VariantProps<typeof badgeVariants> & { asChild?: boolean }) {
  const Comp = asChild ? Slot : "span";

  return (
    <Comp
      data-slot="badge"
      data-variant={variant}
      className={cn(badgeVariants({ variant }), className)}
      {...props}
    />
  );
}

export { Badge, badgeVariants };
