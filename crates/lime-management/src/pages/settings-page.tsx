import { useEffect, useState, type FormEvent, type ReactNode } from "react";
import { Check, LoaderCircle, RotateCcw } from "lucide-react";
import type { Config } from "@/api/types";
import { loadModel, setConfig } from "@/api/commands";
import { useManagement } from "@/app/management-context";
import { useTheme, type Theme } from "@/app/theme-context";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { Switch } from "@/components/ui/switch";
import { ErrorState } from "@/components/page-parts";
import { ModelManager } from "@/components/model-manager";
import { ModelMemory } from "@/components/model-memory";
import { errorMessage } from "@/ui/format";

const schemas = [
  ["rime_ice", "雾凇拼音"], ["double_pinyin", "自然码双拼"], ["double_pinyin_abc", "智能 ABC 双拼"],
  ["double_pinyin_mspy", "微软双拼"], ["double_pinyin_sogou", "搜狗双拼"], ["double_pinyin_flypy", "小鹤双拼"],
  ["double_pinyin_ziguang", "紫光双拼"], ["double_pinyin_jiajia", "拼音加加双拼"],
];
function SettingRow({ label, id, children }: { label: string; id: string; children: ReactNode }) {
  return <div className="setting-row"><Label htmlFor={id} className="font-normal leading-relaxed">{label}</Label>{children}</div>;
}

export function SettingsPage() {
  const { config, status, pending, run, connected, notify } = useManagement();
  const { theme, setTheme } = useTheme();
  // A draft is created only by an edit. Polling cannot overwrite it.
  const [draft, setDraft] = useState<Config | null>(null);
  const [validation, setValidation] = useState<string | null>(null);
  const values = draft ?? config;
  const busy = !!pending;
  const changed = !!draft && JSON.stringify(draft) !== JSON.stringify(config);
  useEffect(() => {
    if (draft && config && JSON.stringify(draft) === JSON.stringify(config)) setDraft(null);
  }, [draft, config]);
  const update = <K extends keyof Config>(key: K, value: Config[K]) => {
    if (values) setDraft({ ...values, [key]: value });
    setValidation(null);
  };
  function numberField(key: keyof Config, label: string, min: number, max: number) {
    return <SettingRow key={key} label={label} id={key}>
      <Input id={key} className="w-24 text-right tabular-nums" type="number" min={min} max={max} step="1" required disabled={!values || busy || !connected}
        value={values && Number.isFinite(values[key]) ? Number(values[key]) : ""}
        onChange={(event) => update(key, event.target.value === "" ? NaN : event.target.valueAsNumber)} />
    </SettingRow>;
  }
  async function save(event: FormEvent) {
    event.preventDefault();
    if (!values || !config || busy || !connected) return;
    if (values.context_preview_char_limit > values.preceding_text_char_limit) return setValidation("前文预览长度不能超过前文读取长度");
    if (values.llm_effective_count > values.llm_rerank_count) return setValidation("采纳候选数不能超过重排候选数");
    setValidation(null);
    const path = status?.model.loaded ? status.model.path : null;
    const needsReload = path && (["llm_backend", "llm_context_token_limit", "llm_rerank_count", "llm_inference_count_limit"] as const).some((key) => config[key] !== values[key]);
    await run("保存设置", async () => {
      const saved = await setConfig(values);
      setDraft(saved.config);
      if (needsReload && path) {
        try { await loadModel(path); }
        catch (cause) { throw new Error("设置已保存，模型重载失败：" + errorMessage(cause)); }
      }
      notify(needsReload ? "设置已保存，模型已重载" : "设置已保存", "success");
      // Keep the saved snapshot until the core refresh returns; no old value flicker.
    });
  }

  return <div className="settings-layout">
    <form className="settings-form page-stack" onSubmit={(event) => void save(event)} aria-label="输入设置">
      <Card variant="rows" className="divide-y">
        <SettingRow label="推理设备" id="llm_backend"><NativeSelect id="llm_backend" className="w-40" value={values?.llm_backend ?? "cuda"} disabled={!values || busy || !connected} onChange={(event) => update("llm_backend", event.target.value as Config["llm_backend"])}>
          <NativeSelectOption value="cuda">GPU · CUDA</NativeSelectOption><NativeSelectOption value="cpu">CPU</NativeSelectOption>
        </NativeSelect></SettingRow>
        <div className="settings-field-pair">
          {numberField("llm_rerank_count", "重排候选数", 1, 128)}
          {numberField("llm_effective_count", "采纳候选数", 1, 32)}
          {numberField("llm_context_token_limit", "单次 Token 上限", 1, 4096)}
          {numberField("llm_inference_count_limit", "单次输入推理上限", 1, 32)}
        </div>
        <SettingRow label="重排时忽略 Emoji" id="llm_ignore_emoji"><Switch id="llm_ignore_emoji" checked={values?.llm_ignore_emoji ?? true} disabled={!values || busy || !connected} onCheckedChange={(checked) => update("llm_ignore_emoji", checked)} /></SettingRow>
      </Card>
      <Card variant="rows" className="settings-field-pair">
        {numberField("preceding_text_char_limit", "前文读取长度", 1, 4096)}
        {numberField("context_preview_char_limit", "前文预览长度", 0, 1024)}
      </Card>
      <Card variant="rows" className="divide-y">
        <SettingRow label="输入方案" id="rime_schema"><NativeSelect id="rime_schema" className="w-40" value={values?.rime_schema ?? "rime_ice"} disabled={!values || busy || !connected} onChange={(event) => update("rime_schema", event.target.value)}>
          {values && !schemas.some(([key]) => key === values.rime_schema) && <NativeSelectOption value={values.rime_schema}>{values.rime_schema}</NativeSelectOption>}
          {schemas.map(([key, label]) => <NativeSelectOption key={key} value={key}>{label}</NativeSelectOption>)}
        </NativeSelect></SettingRow>
        {numberField("page_size", "每页候选数", 1, 20)}
        <SettingRow label="外观" id="theme"><NativeSelect id="theme" className="w-40" value={theme} onChange={(event) => setTheme(event.target.value as Theme)}>
          <NativeSelectOption value="system">跟随系统</NativeSelectOption><NativeSelectOption value="light">浅色</NativeSelectOption><NativeSelectOption value="dark">深色</NativeSelectOption>
        </NativeSelect></SettingRow>
      </Card>
      {validation && <ErrorState message={validation} />}
      <div className="sticky bottom-0 flex flex-wrap items-center justify-end gap-3 border-t bg-background/95 py-4">
        {changed && <><span className="mr-auto text-xs text-muted-foreground">未保存</span><Button variant="ghost" type="button" disabled={busy} onClick={() => { setDraft(null); setValidation(null); }}><RotateCcw />撤销修改</Button></>}
        <Button type="submit" disabled={!changed || busy || !connected}>{pending === "保存设置" ? <LoaderCircle className="animate-spin" /> : <Check />}保存设置</Button>
      </div>
    </form>
    <div className="page-stack">
      <aside aria-label="模型内存与显存占用"><ModelMemory model={status?.model ?? null} connected={connected} /></aside>
      <ModelManager />
    </div>
  </div>;
}
