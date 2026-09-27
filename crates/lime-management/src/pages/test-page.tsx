import { useEffect, useRef, useState, type FormEvent } from "react";
import { FlaskConical, LoaderCircle, Play, RotateCcw } from "lucide-react";
import { testInput } from "@/api/commands";
import type { InputData } from "@/api/types";
import { useManagement } from "@/app/management-context";
import { InputResult } from "@/components/input-result";
import { EmptyState, ErrorState, Field } from "@/components/page-parts";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { errorMessage } from "@/ui/format";
import { signalInputTestCompleted } from "./history-state";

export function TestPage({ active }: { active: boolean }) {
  const { config, connected, pending, run } = useManagement();
  const [context, setContext] = useState("");
  const [preedit, setPreedit] = useState("");
  const [result, setResult] = useState<{ data: InputData; effectiveCount: number } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [testing, setTesting] = useState(false);
  const epoch = useRef(0);
  const inFlight = useRef(false);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; epoch.current += 1; };
  }, []);

  function changeInput(update: () => void) {
    epoch.current += 1;
    update();
    setResult(null);
    setError(null);
  }

  function clear() {
    changeInput(() => { setContext(""); setPreedit(""); });
  }

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (inFlight.current || pending) return;
    const pinyin = preedit.trim();
    if (!pinyin) { setError("请输入拼音"); return; }
    const requestEpoch = ++epoch.current;
    const effectiveCount = config?.llm_effective_count ?? 3;
    inFlight.current = true;
    setTesting(true);
    setError(null);
    setResult(null);
    try {
      const response = await run("输入测试", async () => {
        try {
          const data = await testInput(context, pinyin);
          signalInputTestCompleted();
          return data;
        } catch (cause) {
          if (mounted.current && epoch.current === requestEpoch) setError(errorMessage(cause));
          throw cause;
        }
      });
      if (response.ok && mounted.current && epoch.current === requestEpoch) {
        setResult({ data: response.value, effectiveCount });
      }
    } finally {
      inFlight.current = false;
      if (mounted.current) setTesting(false);
    }
  }

  return (
    <div className="space-y-6" aria-busy={testing}>
      <Card>
        <CardContent>
          <form className="space-y-5" onSubmit={(event) => { void submit(event); }}>
            <Field label="上文" htmlFor="test-context"><Textarea id="test-context" rows={4} value={context} onChange={(event) => changeInput(() => setContext(event.target.value))} className="resize-y" /></Field>
            <Field label="拼音" htmlFor="test-preedit"><Input id="test-preedit" value={preedit} autoComplete="off" autoCapitalize="off" spellCheck={false} onChange={(event) => changeInput(() => setPreedit(event.target.value))} className="font-mono" /></Field>
            <div className="flex flex-wrap items-center justify-end gap-2">
              <Button type="button" variant="ghost" onClick={clear} disabled={!context && !preedit && !result && !error}><RotateCcw className="size-4" aria-hidden="true" />清空</Button>
              <Button type="submit" disabled={!active || !connected || Boolean(pending) || testing || !preedit.trim()}>{testing ? <LoaderCircle className="size-4 animate-spin" aria-hidden="true" /> : <Play className="size-4" aria-hidden="true" />}{testing ? "测试中" : "测试"}</Button>
            </div>
          </form>
        </CardContent>
      </Card>
      {error && <ErrorState message={error} />}
      {result ? <InputResult data={result.data} effectiveCount={result.effectiveCount} /> : !error && <EmptyState icon={FlaskConical} label={testing ? "正在获取候选" : "暂无测试结果"} />}
    </div>
  );
}
