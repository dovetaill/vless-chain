import * as React from 'react';
import { cn } from '@/lib/utils';
import { cva, type VariantProps } from 'class-variance-authority';
import { Slot as SlotPrimitive } from 'radix-ui';

const badgeVariants = cva(
  'inline-flex items-center justify-center border border-transparent font-medium focus:outline-hidden focus:ring-2 focus:ring-ring focus:ring-offset-2 [&_svg]:-ms-px [&_svg]:shrink-0',
  {
    variants: {
      variant: {
        primary: 'bg-primary text-primary-foreground',
        secondary: 'bg-secondary text-secondary-foreground',
        success: 'bg-emerald-600 text-white',
        warning: 'bg-amber-500 text-white',
        info: 'bg-violet-600 text-white',
        outline: 'bg-transparent border border-border text-secondary-foreground',
        destructive: 'bg-destructive text-destructive-foreground',
      },
      appearance: {
        default: '',
        light: '',
        outline: '',
      },
      size: {
        md: 'rounded-md px-[0.45rem] h-6 min-w-6 gap-1.5 text-xs [&_svg]:size-3.5',
        sm: 'rounded-sm px-[0.325rem] h-5 min-w-5 gap-1 text-[0.6875rem] leading-[0.75rem] [&_svg]:size-3',
        xs: 'rounded-sm px-[0.25rem] h-4 min-w-4 gap-1 text-[0.625rem] leading-[0.5rem] [&_svg]:size-3',
      },
    },
    compoundVariants: [
      {
        variant: 'primary',
        appearance: 'light',
        className: 'text-blue-700 bg-blue-50 dark:bg-blue-500/15 dark:text-blue-300',
      },
      {
        variant: 'secondary',
        appearance: 'light',
        className: 'bg-secondary dark:bg-secondary/70 text-secondary-foreground',
      },
      {
        variant: 'success',
        appearance: 'light',
        className: 'text-emerald-700 bg-emerald-50 dark:bg-emerald-500/15 dark:text-emerald-300',
      },
      {
        variant: 'warning',
        appearance: 'light',
        className: 'text-amber-700 bg-amber-50 dark:bg-amber-500/15 dark:text-amber-300',
      },
      {
        variant: 'info',
        appearance: 'light',
        className: 'text-violet-700 bg-violet-50 dark:bg-violet-500/15 dark:text-violet-300',
      },
      {
        variant: 'destructive',
        appearance: 'light',
        className: 'text-red-700 bg-red-50 dark:bg-red-500/15 dark:text-red-300',
      },
      {
        variant: 'primary',
        appearance: 'outline',
        className:
          'text-blue-700 border-blue-200 bg-blue-50/70 dark:bg-blue-500/10 dark:border-blue-500/30 dark:text-blue-300',
      },
      {
        variant: 'success',
        appearance: 'outline',
        className:
          'text-emerald-700 border-emerald-200 bg-emerald-50/70 dark:bg-emerald-500/10 dark:border-emerald-500/30 dark:text-emerald-300',
      },
      {
        variant: 'warning',
        appearance: 'outline',
        className:
          'text-amber-700 border-amber-200 bg-amber-50/70 dark:bg-amber-500/10 dark:border-amber-500/30 dark:text-amber-300',
      },
      {
        variant: 'info',
        appearance: 'outline',
        className:
          'text-violet-700 border-violet-200 bg-violet-50/70 dark:bg-violet-500/10 dark:border-violet-500/30 dark:text-violet-300',
      },
      {
        variant: 'destructive',
        appearance: 'outline',
        className:
          'text-red-700 border-red-200 bg-red-50/70 dark:bg-red-500/10 dark:border-red-500/30 dark:text-red-300',
      },
    ],
    defaultVariants: {
      variant: 'primary',
      appearance: 'default',
      size: 'md',
    },
  },
);

function Badge({
  className,
  variant,
  size,
  appearance,
  asChild = false,
  ...props
}: React.ComponentProps<'span'> & VariantProps<typeof badgeVariants> & { asChild?: boolean }) {
  const Comp = asChild ? SlotPrimitive.Slot : 'span';

  return (
    <Comp
      data-slot="badge"
      className={cn(badgeVariants({ variant, size, appearance }), className)}
      {...props}
    />
  );
}

export { Badge };
