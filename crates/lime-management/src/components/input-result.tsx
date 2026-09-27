import { diagnosticsFrom } from "@/api/decode";
import type { BoundaryRollback, Candidate, CandidateDiagnostic, InputData } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent } from "@/components/ui/card";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { formatDecimal, formatLogprobs, formatMilliseconds } from "@/ui/format";

const serviceLabels = {
  ready: "已完成",
  rime_only: "基础模式",
  reloading: "重载中",
  unavailable: "服务不可用",
} as const;

function candidateText(candidate: Candidate | null) {
  return candidate ? candidate.displayText || candidate.commitText : "—";
}

function displayCandidate(diagnostic: CandidateDiagnostic, index: number, data: InputData, effectiveCount: number) {
  if (diagnostic.hasDisplayCandidate) return diagnostic.displayCandidate;
  if (data.diagnostics.length === 0 || index < Math.max(0, Math.trunc(effectiveCount))) {
    return diagnostic.llmCandidate ?? data.finalCandidates[index] ?? null;
  }
  return null;
}

function RollbackDetail({ rollback }: { rollback: BoundaryRollback | null | undefined }) {
  if (!rollback || rollback.replayedTokenCount <= 0) return null;
  const hasText = rollback.prefixText !== null && rollback.replayedText !== null;
  return (
    <div className="space-y-3 rounded-lg border bg-muted/40 p-4" data-boundary-rollback>
      <dl className="flex flex-wrap items-center gap-x-6 gap-y-2 text-sm">
        <div className="flex items-center gap-2"><dt className="text-muted-foreground">回退 Token</dt><dd className="font-mono tabular-nums">{rollback.replayedTokenCount}</dd></div>
        <div className="flex items-center gap-2"><dt className="text-muted-foreground">评分起点</dt><dd>{rollback.prefixTokenCount === 0 ? "上文开头" : `第 ${rollback.prefixTokenCount} 个 Token 后`}</dd></div>
      </dl>
      {hasText && <p className="whitespace-pre-wrap break-all font-mono text-sm">{rollback.prefixText}<span className="font-semibold text-primary" role="img" aria-label="评分起点" title="评分起点">│</span><span className="font-semibold text-primary">{rollback.replayedText}</span></p>}
    </div>
  );
}

export function InputResult({ data, effectiveCount = 3 }: { data: InputData; effectiveCount?: number }) {
  const diagnostics = data.diagnostics.length ? data.diagnostics : diagnosticsFrom([], data.rimeCandidates, data.finalCandidates);
  const performance = data.llmPerformance;
  const metrics: [string, string | number][] = [
    ["端到端用时", formatMilliseconds(data.endToEndMs)],
    ["Rime 用时", formatMilliseconds(data.rimeMs ?? performance?.rimeMs)],
    ["推理用时", formatMilliseconds(performance?.decodeMs)],
    ["总评分用时", formatMilliseconds(performance?.totalMs)],
    ["分词用时", formatMilliseconds(performance?.tokenizeMs)],
    ["送入候选", performance?.candidateCount ?? "—"],
    ["返回得分", performance?.scoredCount ?? "—"],
    ["目标 Token", performance?.targetTokenCount ?? "—"],
    ["解码批次", performance?.batchCount ?? "—"],
    ["边界不匹配", performance?.mismatchCount ?? "—"],
    ["上下文 Token", performance?.contextTokenCount ?? "—"],
    ["Decode 输入行", performance?.decodeInputTokenCount ?? "—"],
    ["Logprob 输出行", performance?.logprobOutputCount ?? "—"],
    ["推理次数上限", performance?.inferenceCountLimit ?? "—"],
    ["未评分候选", performance?.omittedCandidateCount ?? "—"],
    ["使用上文", data.contextUsed === null ? "—" : data.contextUsed ? "是" : "否"],
  ];

  return (
    <Card className="min-w-0 overflow-hidden">
      <CardContent className="space-y-5">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div className="flex min-w-0 items-center gap-3"><Badge variant="secondary">{serviceLabels[data.serviceState]}</Badge><span className="truncate font-mono text-sm">{data.preedit || "—"}</span></div>
          <span className="shrink-0 font-mono text-sm tabular-nums text-muted-foreground" aria-label="端到端用时">{formatMilliseconds(data.endToEndMs)}</span>
        </div>
        <Table aria-label="候选结果">
          <TableHeader><TableRow><TableHead className="w-12">#</TableHead><TableHead>Rime 候选</TableHead><TableHead>排序候选</TableHead><TableHead className="text-right">Logprob</TableHead><TableHead>Logprobs</TableHead><TableHead>边界不匹配</TableHead><TableHead>展示候选</TableHead></TableRow></TableHeader>
          <TableBody>
            {diagnostics.length ? diagnostics.map((diagnostic, index) => {
              const scored = diagnostic.llmCandidate !== null && diagnostic.logprobs.length > 0 && diagnostic.logprob !== null;
              return <TableRow key={index}>
                <TableCell className="font-mono tabular-nums text-muted-foreground">{index + 1}</TableCell>
                <TableCell className="min-w-24 whitespace-normal break-all">{candidateText(diagnostic.rimeCandidate)}</TableCell>
                <TableCell className="min-w-24 whitespace-normal break-all">{candidateText(diagnostic.llmCandidate)}</TableCell>
                <TableCell className="text-right font-mono tabular-nums">{scored ? formatDecimal(diagnostic.logprob!) : "—"}</TableCell>
                <TableCell className="min-w-40 whitespace-normal break-all font-mono text-xs">{scored ? formatLogprobs(diagnostic.logprobs, diagnostic.logprob) : "—"}</TableCell>
                <TableCell className={scored && diagnostic.mismatch ? "text-destructive" : "text-muted-foreground"}>{!scored || diagnostic.mismatch === null ? "—" : diagnostic.mismatch ? "是" : "否"}</TableCell>
                <TableCell className="min-w-24 whitespace-normal break-all font-medium">{candidateText(displayCandidate(diagnostic, index, data, effectiveCount))}</TableCell>
              </TableRow>;
            }) : <TableRow><TableCell colSpan={7} className="h-24 text-center text-muted-foreground">暂无候选</TableCell></TableRow>}
          </TableBody>
        </Table>
        <dl className="grid grid-cols-1 gap-x-8 gap-y-3 border-t pt-4 sm:grid-cols-2 xl:grid-cols-3">
          {metrics.map(([label, value]) => <div key={label} className="flex items-center justify-between gap-3 text-sm"><dt className="text-muted-foreground">{label}</dt><dd className="font-mono tabular-nums">{value}</dd></div>)}
        </dl>
        <RollbackDetail rollback={performance?.boundaryRollback} />
      </CardContent>
    </Card>
  );
}
