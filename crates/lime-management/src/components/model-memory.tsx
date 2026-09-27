import type { ModelInfo } from "@/api/types";
import { Card } from "@/components/ui/card";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { formatBytes } from "@/ui/format";
import { buildModelMemoryTable } from "./model-memory-data";

export function ModelMemory({ model, connected }: { model: ModelInfo | null; connected: boolean }) {
  const memory = buildModelMemoryTable(connected && model?.loaded ? model : null);
  const status = !connected ? "服务未连接" : !model?.loaded ? "未加载模型" : !memory.hasData ? "用量暂不可用" : null;
  return <Card variant="rows">
    {status && <p role="status" className="border-b px-3 py-3 text-sm text-muted-foreground">{status}</p>}
    <Table aria-label="模型内存占用">
      <TableHeader>
        <TableRow>
          <TableHead scope="col" className="px-3 text-xs">内存分配</TableHead>
          {memory.devices.map((device) => <TableHead key={device.id} scope="col" className="px-3 text-right text-xs">{device.label}</TableHead>)}
        </TableRow>
      </TableHeader>
      <TableBody>
        {memory.rows.map((row) => <TableRow key={row.id}>
          <TableHead scope="row" className="h-auto px-3 py-2.5 text-xs text-muted-foreground">{row.label}</TableHead>
          {row.values.map((bytes, index) => <TableCell key={memory.devices[index].id} className="px-3 py-2.5 text-right font-mono text-xs tabular-nums">{bytes === null ? "—" : formatBytes(bytes)}</TableCell>)}
        </TableRow>)}
      </TableBody>
    </Table>
    <dl className="flex items-center justify-between gap-3 border-t px-3 py-3 text-xs">
      <dt className="text-muted-foreground">已报告合计</dt>
      <dd className="font-mono font-medium tabular-nums">{memory.totalBytes === null ? "—" : formatBytes(memory.totalBytes)}</dd>
    </dl>
  </Card>;
}
