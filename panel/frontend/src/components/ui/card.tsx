import * as React from 'react';
import { cn } from '@/lib/utils';

function Card({ className, ...props }: React.HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      data-slot="card"
      className={cn(
        'flex min-w-0 flex-col items-stretch rounded-xl border border-border bg-card text-card-foreground shadow-xs shadow-black/5',
        className,
      )}
      {...props}
    />
  );
}

function CardHeader({ className, ...props }: React.HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      data-slot="card-header"
      className={cn('flex min-h-14 flex-wrap items-center justify-between gap-2.5 border-b border-border px-5 py-4', className)}
      {...props}
    />
  );
}

function CardContent({ className, ...props }: React.HTMLAttributes<HTMLDivElement>) {
  return <div data-slot="card-content" className={cn('grow p-5', className)} {...props} />;
}

function CardTable({ className, ...props }: React.HTMLAttributes<HTMLDivElement>) {
  return <div data-slot="card-table" className={cn('grid grow', className)} {...props} />;
}

function CardFooter({ className, ...props }: React.HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      data-slot="card-footer"
      className={cn('flex min-h-14 items-center border-t border-border px-5', className)}
      {...props}
    />
  );
}

function CardHeading({ className, ...props }: React.HTMLAttributes<HTMLDivElement>) {
  return <div data-slot="card-heading" className={cn('space-y-1', className)} {...props} />;
}

function CardToolbar({ className, ...props }: React.HTMLAttributes<HTMLDivElement>) {
  return <div data-slot="card-toolbar" className={cn('flex items-center gap-2.5', className)} {...props} />;
}

function CardTitle({ className, ...props }: React.HTMLAttributes<HTMLHeadingElement>) {
  return (
    <h3
      data-slot="card-title"
      className={cn('min-w-0 break-words text-base font-semibold leading-5 tracking-tight', className)}
      {...props}
    />
  );
}

export { Card, CardContent, CardFooter, CardHeader, CardHeading, CardTable, CardTitle, CardToolbar };
