import { useId, type ReactNode } from "react";
import { AlertCircle, ChevronLeft, ChevronRight, Inbox, LoaderCircle, type LucideIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog";

export function Toolbar({ children }: { children: ReactNode }) {
  return <div className="flex flex-wrap items-center justify-between gap-3">{children}</div>;
}
export function Field({ label, htmlFor, children }: { label: string; htmlFor?: string; children: ReactNode }) {
  const id = useId();
  return <div className="flex min-w-0 flex-col gap-2" role={htmlFor ? undefined : "group"} aria-labelledby={htmlFor ? undefined : id}>
    <Label id={id} htmlFor={htmlFor}>{label}</Label>{children}
  </div>;
}
export function EmptyState({ icon: Icon = Inbox, label, action }: { icon?: LucideIcon; label: string; action?: ReactNode }) {
  return <div className="flex flex-col items-center justify-center gap-4 rounded-xl border border-dashed py-16 text-muted-foreground">
    <div className="flex size-12 items-center justify-center rounded-xl bg-muted"><Icon className="size-6" aria-hidden /></div>
    <span>{label}</span>{action}
  </div>;
}
export function ErrorState({ message, onRetry }: { message: string; onRetry?: () => void }) {
  return <div role="alert" className="flex flex-wrap items-center gap-3 rounded-lg border border-destructive/20 bg-destructive/5 px-4 py-3">
    <AlertCircle className="size-4 shrink-0 text-destructive" aria-hidden /><span className="min-w-0 flex-1 break-words">{message}</span>
    {onRetry && <Button variant="outline" size="sm" onClick={onRetry}>重试</Button>}
  </div>;
}
export function Pagination({ page, total, pageSize, onPageChange, disabled }: { page: number; total: number; pageSize: number; onPageChange: (page: number) => void; disabled?: boolean }) {
  const pages = Math.max(1, Math.ceil(total / pageSize));
  return <div className="flex items-center justify-between gap-4 py-1">
    <span className="text-xs text-muted-foreground">共 {total.toLocaleString("zh-CN")} 条</span>
    <div className="flex items-center gap-3">
      <Button size="icon-sm" variant="outline" aria-label="上一页" disabled={disabled || page <= 1} onClick={() => onPageChange(page - 1)}><ChevronLeft /></Button>
      <span className="text-xs tabular-nums" aria-live="polite">{page} / {pages}</span>
      <Button size="icon-sm" variant="outline" aria-label="下一页" disabled={disabled || page >= pages} onClick={() => onPageChange(page + 1)}><ChevronRight /></Button>
    </div>
  </div>;
}
export function ConfirmDialog({ open, onOpenChange, title, description, confirmLabel = "确认", pending = false, onConfirm }: {
  open: boolean; onOpenChange: (open: boolean) => void; title: string; description?: string; confirmLabel?: string; pending?: boolean; onConfirm: () => void;
}) {
  return <AlertDialog open={open} onOpenChange={(next) => { if (!pending) onOpenChange(next); }}>
    <AlertDialogContent {...(!description ? { "aria-describedby": undefined } : {})}>
      <AlertDialogHeader><AlertDialogTitle>{title}</AlertDialogTitle>{description && <AlertDialogDescription>{description}</AlertDialogDescription>}</AlertDialogHeader>
      <AlertDialogFooter><AlertDialogCancel disabled={pending}>取消</AlertDialogCancel>
        <AlertDialogAction variant="destructive" disabled={pending} onClick={(event) => { event.preventDefault(); onConfirm(); }}>{pending && <LoaderCircle className="animate-spin" />}{confirmLabel}</AlertDialogAction>
      </AlertDialogFooter>
    </AlertDialogContent>
  </AlertDialog>;
}
