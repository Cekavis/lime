import { useState, type FormEvent } from "react";
import { Box, Check, FilePlus2, Info, LoaderCircle, Pencil, Plus, Power, Trash2 } from "lucide-react";
import type { ModelPreset } from "@/api/types";
import { deleteModelPreset, renameModelPreset, saveModelPreset, selectModelPreset, unloadModel } from "@/api/commands";
import { useManagement } from "@/app/management-context";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import { Dialog, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { ConfirmDialog, EmptyState, ErrorState, Field, Toolbar } from "@/components/page-parts";
import { formatBytes } from "@/ui/format";

export function ModelManager() {
  const { status, presets, pending, connected, run, error, refresh } = useManagement();
  const [editing, setEditing] = useState<ModelPreset | "new" | null>(null);
  const [removing, setRemoving] = useState<ModelPreset | null>(null);
  const [details, setDetails] = useState<ModelPreset | null>(null);
  const [name, setName] = useState("");
  const [path, setPath] = useState("");
  const busy = !!pending;
  const loaded = status?.model.loaded;
  function edit(preset: ModelPreset | "new") {
    setEditing(preset);
    setName(preset === "new" ? "" : preset.name);
    setPath(preset === "new" ? "" : preset.path);
  }
  async function save(event: FormEvent) {
    event.preventDefault();
    if (!editing || !name.trim() || !path.trim()) return;
    const result = await run("保存模型", () => editing === "new" ? saveModelPreset(name.trim(), path.trim()) : renameModelPreset(editing.name, name.trim()), "模型已保存");
    if (result.ok) setEditing(null);
  }
  return <div className="page-stack">
    <Toolbar>
      <Badge variant="secondary">{presets.length} 个模型</Badge>
      <div className="flex flex-wrap items-center gap-2">
        {loaded && <Button variant="outline" disabled={busy || !connected} onClick={() => void run("卸载模型", unloadModel, "模型已卸载")}><Power />卸载模型</Button>}
        <Button disabled={busy || !connected} onClick={() => edit("new")}><Plus />添加模型</Button>
      </div>
    </Toolbar>
    {connected && error && <ErrorState message={error} onRetry={() => void refresh()} />}
    {!presets.length ? <EmptyState icon={Box} label="暂无模型" action={<Button variant="outline" disabled={busy || !connected} onClick={() => edit("new")}><FilePlus2 />添加模型</Button>} /> :
      <Card variant="rows" className="divide-y">{presets.map((preset) => {
        const active = !!loaded && status.model.path === preset.path;
        return <div key={preset.name} className="flex min-w-0 flex-wrap items-center gap-3 p-4 sm:p-5">
          <div className="flex size-10 shrink-0 items-center justify-center rounded-lg bg-secondary text-primary"><Box className="size-5" aria-hidden /></div>
          <div className="min-w-0 flex-1"><span className="block truncate font-medium" title={preset.name}>{preset.name}</span></div>
          {active && <Badge variant="secondary" className="hidden sm:inline-flex"><Check />使用中</Badge>}
          <div className="ml-auto flex shrink-0 items-center gap-1">
            <Button variant={active ? "ghost" : "outline"} size="sm" disabled={active || busy || !connected} onClick={() => void run("切换模型", () => selectModelPreset(preset.name), "模型已切换")}>{pending === "切换模型" ? <LoaderCircle className="animate-spin" /> : null}{active ? "已加载" : "加载"}</Button>
            <Button variant="ghost" size="icon-sm" aria-label={`查看 ${preset.name} 的文件信息`} onClick={() => setDetails(preset)}><Info /></Button>
            <Button variant="ghost" size="icon-sm" aria-label={`重命名 ${preset.name}`} disabled={busy || !connected} onClick={() => edit(preset)}><Pencil /></Button>
            <Button variant="ghost" size="icon-sm" aria-label={`删除 ${preset.name}`} disabled={busy || !connected} onClick={() => setRemoving(preset)}><Trash2 /></Button>
          </div>
        </div>;
      })}</Card>}
    <Dialog open={!!details} onOpenChange={(open) => { if (!open) setDetails(null); }}>
      <DialogContent aria-describedby={undefined}>
        <DialogHeader><DialogTitle className="break-all pr-6">{details?.name}</DialogTitle></DialogHeader>
        <dl className="grid gap-4 text-sm">
          <div className="flex gap-4"><dt className="shrink-0 text-muted-foreground">路径</dt><dd className="min-w-0 break-all font-mono">{details?.path}</dd></div>
          {details?.sizeBytes != null && <div className="flex gap-4"><dt className="text-muted-foreground">大小</dt><dd>{formatBytes(details.sizeBytes)}</dd></div>}
          {loaded && details?.path === status.model.path && status.model.scoringPath && <div className="flex gap-4"><dt className="text-muted-foreground">推理方式</dt><dd>{status.model.scoringPath === "attention" ? "Attention" : "Recurrent"}</dd></div>}
        </dl>
      </DialogContent>
    </Dialog>
    <Dialog open={editing !== null} onOpenChange={(open) => { if (!open && !busy) setEditing(null); }}>
      <DialogContent aria-describedby={undefined} showCloseButton={!busy}>
        <DialogHeader><DialogTitle>{editing === "new" ? "添加模型" : "重命名模型"}</DialogTitle></DialogHeader>
        <form className="flex flex-col gap-5" onSubmit={(event) => void save(event)}>
          <Field label="名称" htmlFor="model-name"><Input id="model-name" value={name} onChange={(event) => setName(event.target.value)} required disabled={busy} autoComplete="off" /></Field>
          {editing === "new" && <Field label="GGUF 文件路径" htmlFor="model-path"><Input id="model-path" value={path} onChange={(event) => setPath(event.target.value)} required disabled={busy} placeholder="C:\Models\model.gguf" autoComplete="off" /></Field>}
          <DialogFooter><Button type="button" variant="outline" disabled={busy} onClick={() => setEditing(null)}>取消</Button><Button type="submit" disabled={busy || !name.trim() || !path.trim()}>{busy && <LoaderCircle className="animate-spin" />}保存</Button></DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
    <ConfirmDialog open={!!removing} onOpenChange={(open) => { if (!open) setRemoving(null); }} title={`删除“${removing?.name ?? ""}”？`} confirmLabel="删除" pending={busy} onConfirm={() => {
      if (removing) void run("删除模型", () => deleteModelPreset(removing.name), "模型已删除").then((result) => { if (result.ok) setRemoving(null); });
    }} />
  </div>;
}
