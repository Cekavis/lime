import { useRef, useState } from "react";
import { BookOpen, Download, LoaderCircle, Trash2, Upload } from "lucide-react";
import { clearDictionary, getDictionaryPage, importDictionary } from "@/api/commands";
import { DICTIONARY_PAGE_SIZE, type DictionaryEntry } from "@/api/types";
import { useManagement } from "@/app/management-context";
import { usePollingResource } from "@/hooks/use-polling-resource";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Card } from "@/components/ui/card";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { ConfirmDialog, EmptyState, ErrorState, Pagination, Toolbar } from "@/components/page-parts";
import { downloadJson } from "@/lib/download";
import { parseDictionary } from "@/lib/dictionary";
import { errorMessage } from "@/ui/format";

export function DictionaryPage({ active }: { active: boolean }) {
  const { connected, pending, run, notify } = useManagement();
  const [page, setPage] = useState(1);
  const [confirm, setConfirm] = useState(false);
  const fileInput = useRef<HTMLInputElement>(null);
  const resource = usePollingResource({ load: async () => {
    const result = await getDictionaryPage(page);
    const lastPage = Math.max(1, Math.ceil(result.total / result.pageSize));
    return result.page > lastPage ? getDictionaryPage(lastPage) : result;
  }, active: active && !pending, key: String(page) });
  const { data, error, loading } = resource;
  const busy = !!pending;
  const visiblePage = data?.page ?? page;
  async function importFile(file: File) {
    try {
      const entries = parseDictionary(await file.text());
      resource.invalidate();
      const result = await run("导入词库", () => importDictionary(entries), `已导入 ${entries.length} 条词条`);
      resource.invalidate();
      if (result.ok) setPage(1);
      await resource.refresh();
    } catch (cause) { notify(errorMessage(cause), "error"); }
  }
  async function exportAll() {
    await run("导出词库", async () => {
      const first = await getDictionaryPage(1);
      const entries: DictionaryEntry[] = [...first.items];
      const pages = Math.ceil(first.total / first.pageSize);
      for (let current = 2; current <= pages; current++) entries.push(...(await getDictionaryPage(current)).items);
      downloadJson("lime-dictionary.json", entries);
    }, "词库已导出");
  }
  return <div className="page-stack">
    <Toolbar><Badge variant="secondary">{data?.total ?? 0} 条词条</Badge>
      <div className="flex flex-wrap items-center gap-2">
        <Button variant="ghost" size="icon" aria-label="清空词库" disabled={!data?.total || busy || !connected} onClick={() => setConfirm(true)}><Trash2 /></Button>
        <Button variant="outline" disabled={!data?.total || busy || !connected} onClick={() => void exportAll()}><Download />导出</Button>
        <Button disabled={busy || !connected} onClick={() => fileInput.current?.click()}>{pending === "导入词库" ? <LoaderCircle className="animate-spin" /> : <Upload />}导入</Button>
        <input className="hidden" ref={fileInput} type="file" accept=".json,application/json" aria-label="导入词库文件" onChange={(event) => {
          const file = event.target.files?.[0];
          event.target.value = "";
          if (file) void importFile(file);
        }} />
      </div>
    </Toolbar>
    {error && <ErrorState message={error} onRetry={() => void resource.refresh()} />}
    {!data && !error ? <EmptyState icon={LoaderCircle} label="正在读取词库…" /> : data?.items.length ? <>
      <Card variant="rows"><Table><TableHeader><TableRow><TableHead>词条</TableHead><TableHead>拼音</TableHead><TableHead className="text-right">词频</TableHead></TableRow></TableHeader>
        <TableBody>{data.items.map((entry, index) => <TableRow key={`${entry.pinyin}:${entry.text}:${index}`}><TableCell className="font-medium whitespace-normal break-all">{entry.text}</TableCell><TableCell className="font-mono whitespace-normal break-all text-muted-foreground">{entry.pinyin}</TableCell><TableCell className="text-right tabular-nums">{entry.weight}</TableCell></TableRow>)}</TableBody>
      </Table></Card>
      <Pagination page={visiblePage} total={data.total} pageSize={data.pageSize || DICTIONARY_PAGE_SIZE} disabled={busy || loading} onPageChange={setPage} />
    </> : !error && <EmptyState icon={BookOpen} label="词库为空" />}
    <ConfirmDialog open={confirm} onOpenChange={setConfirm} title="清空全部词条？" description="此操作无法撤销。" confirmLabel="清空词库" pending={busy} onConfirm={() => {
      resource.invalidate();
      void run("清空词库", clearDictionary, "词库已清空").then(async (result) => {
        resource.invalidate();
        if (result.ok) { resource.setData({ items: [], total: 0, page: 1, pageSize: DICTIONARY_PAGE_SIZE }); setPage(1); setConfirm(false); }
        await resource.refresh();
      });
    }} />
  </div>;
}
