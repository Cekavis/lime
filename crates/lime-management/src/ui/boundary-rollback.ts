import type { BoundaryRollback } from "../api/types";
import { escapeHtml } from "./format";

export function renderBoundaryRollback(rollback: BoundaryRollback | null | undefined): string {
  if (!rollback || rollback.replayedTokenCount <= 0) return "";
  const replayedCount = rollback.replayedTokenCount;
  if (rollback.prefixText !== null && rollback.replayedText !== null) {
    return '<p class="boundary-rollback" data-boundary-rollback>已回退 ' + replayedCount + ' 个 Token，评分从标记处重新开始；标记后的上文与候选共同计分：' +
      '<span class="boundary-rollback-text mono">' + escapeHtml(rollback.prefixText) +
      '<span class="boundary-rollback-marker" role="img" aria-label="评分起点" title="评分起点">│</span>' +
      '<span class="boundary-rollback-tail">' + escapeHtml(rollback.replayedText) + '</span></span></p>';
  }
  const position = rollback.prefixTokenCount === 0 ? "上文开头" : "上文第 " + rollback.prefixTokenCount + " 个 Token 后";
  return '<p class="boundary-rollback" data-boundary-rollback>评分回退至' + position + '（回退 ' + replayedCount + ' 个 Token）；该处之后的上文与候选共同计分。</p>';
}
