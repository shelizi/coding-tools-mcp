# Coding Tools MCP 反思、學習與歷史檢索閉環改善規劃

> 狀態：Draft / implementation-ready
> 建立日期：2026-09-16
> 主要範圍：Node Agent；完成核心設計後同步 Rust Desktop parity
> 目的：讓已累積的 diagnostics、task outcome、history session 與 recovery 記錄真正形成可驗證、可回放、可檢索、可安全 rollout 的自我改善閉環。

---

## 1. 背景與結論

Coding Tools MCP 現在已經不是「樣本不足」階段。系統累積了大量工具呼叫、錯誤、等待、搜尋、task lifecycle 與 history session 資料，也已經有 KnowledgeStore、ExperienceCompiler、ShadowEvaluator、ValidationGate、CanaryStrategyEngine、ToolEvolutionPlanner 等基礎元件。

目前主要問題是：

```text
大量觀測資料
  -> 少量硬編碼 pattern 被 compiler 辨識
  -> candidate / shadow
  -> validation 需要稀少的 task outcome
  -> tool strategy 長期無法 validated
  -> canary 沒有樣本
  -> 已知低效行為持續出現
  -> 新資料繼續累積，但沒有改變後續行為
```

同時 history 已累積到 180 個 session，但 bootstrap 仍然以固定 recent window 為主，因此另一條記憶鏈路也開始失效：

```text
完整 history 持續增加
  -> bootstrap 只載最近 12 個 session
  -> all_history_summary 也只由這批 recent documents 組成
  -> 舊但高度相關的決策掉出視窗
  -> 沒有正式 relevance retrieval 路徑把它找回
```

因此這次改善的核心不是增加更多 telemetry，而是補齊以下閉環：

```text
observe
  -> normalize
  -> durable ingest / replay
  -> classify
  -> accumulate evidence
  -> shadow
  -> validate by strategy safety class
  -> canary
  -> promote / reject / rollback
  -> observe impact
  -> searchable explanation
```

以及：

```text
history checkpoint
  -> structured retrieval index
  -> recent + relevance retrieval
  -> bounded context restore
  -> retrieval usefulness feedback
```

---

## 2. 2026-09-16 基準資料

以下數據用於確認優先級與後續驗收。執行實作前必須重新建立一次 trusted runtime baseline。

### 2.1 Learning / knowledge 狀態

目前 bootstrap 暴露：

| 指標 | 目前值 | 判讀 |
| --- | ---: | --- |
| tool strategy | 3 | 已有學習結果，但種類少 |
| tool strategy implementation | 1 | 真正可執行 intervention 太少 |
| validated tool strategy | 0 | validation 閉環沒有打通 |
| promoted tool strategy | 0 | 沒有 100% rollout |
| canary impact | 0 | 沒有進入可比較 intervention/control 階段 |
| tool evolution candidate | 3 | evolution pipeline 有資料 |
| validated tool evolution | 2 | tool evolution 比 tool strategy 更容易通過 |
| promoted evolved skill | 0 | skill evolution 尚未形成 production feedback |

目前唯一已註冊的 semantics-preserving tool strategy implementation 為：

```text
search_exact_total_fast_tail
```

對應 `search_text` exact-total 的成本問題。

### 2.2 Node Agent 0.29.35 current-version telemetry

基準查詢顯示：

| 指標 | 目前值 |
| --- | ---: |
| tool calls | 6,726 |
| tool errors | 146 |
| p50 | 317 ms |
| p95 | 21,032 ms |
| max | 793,950 ms |
| response bytes | 約 175.7 MB |

這表示平均錯誤率並不是唯一主要問題；tail latency、response payload 與已知低效操作更值得優先處理。

### 2.3 `search_text`

目前版本：

| 指標 | 目前值 |
| --- | ---: |
| calls | 719 |
| exact-total calls | 659 |
| exact-total 比例 | 91.7% |
| files considered | 370,221 |
| files scanned | 338,602 |
| zero-result calls | 136 |
| early-stop calls | 51 |
| avg | 約 7.24 s |
| p95 | 約 30.1 s |
| max | 約 488 s |

結論：`prefer_bounded_search` 已經被 knowledge compiler 辨識，但實際行為沒有被 rollout 機制改變。

### 2.4 `wait_command`

目前版本：

| 指標 | 目前值 |
| --- | ---: |
| calls | 970 |
| empty wait timeouts | 325 |
| empty timeout 比例 | 33.5% |

這表示 polling cadence 還有明顯改善空間。

### 2.5 Editing

目前版本：

| 工具 | calls | errors | error rate |
| --- | ---: | ---: | ---: |
| `edit` | 549 | 52 | 約 9.5% |
| `apply_patch` | 22 | 18 | 約 81.8% |

`apply_patch` 的現況已足以支持「不應作為一般修改的優先工具」這個假設。

### 2.6 Task lifecycle / baseline capture

目前版本曾出現：

- `start_task` / `finish_task` / `change_summary` 的大型 workspace full baseline capture 約 12–13 分鐘。
- 單次 response 曾達 12–28 MB。
- current-version `start_task` 10 次中 7 次 error。
- current-version `finish_task` 僅 4 次，其中 2 次 error。
- 全歷史 task lifecycle 可觀察到的 `finish_task` 樣本仍非常少，相對於數萬次 tool call 明顯不足。

而 `ValidationGate` 的 tool strategy 條件目前要求：

```text
matched >= 10
AND task outcomes >= 3
AND verified task rate >= 80%
AND task failure rate <= 20%
AND tool failure rate <= 25%
```

這形成系統性互鎖：

```text
full baseline 很慢
 -> agent 少用 task lifecycle / finish_task 樣本少
 -> task outcome 稀少
 -> semantics-preserving strategy 無法 validated
 -> canary 不會啟動
 -> search 等已知低效行為不會自動改善
```

### 2.7 History

目前：

| 指標 | 目前值 |
| --- | ---: |
| history sessions | 180 |
| bootstrap loaded sessions | 12 |
| bootstrap omitted sessions | 168 |
| summary window | 12 |

`all_history_summary` 的產生來源也是這 12 個 recent documents，而不是 180 個 session 的真正全文或 relevance-selected 摘要。

因此「all history」目前語意容易被誤解。

### 2.8 Runtime trust caveat

調查時 Node Agent 回報：

```text
running build: e6465fcc...
workspace HEAD: 4fe4bde8...
runtime trust state: mismatch / untrusted
```

因此任何正式 before/after benchmark 都必須先 rebuild/restart 到 workspace HEAD，否則會把「source 已修但 runtime 尚未更新」誤判為當前缺陷。

---

## 3. 問題樹

### P0-A：Validation gate 與 strategy safety class 沒有對齊

現在 `tool_strategy` 不論改動性質，一律高度依賴 task outcome。

但不同 strategy 的風險本質不同：

- semantics-preserving implementation：理論上不改變 caller-visible semantics，只改變執行方式。
- recovery strategy：可能改變 fallback / retry 流程，但不應改變授權與安全政策。
- behavioral strategy：可能改變工具選擇、預設參數或控制流程，風險較高。
- evolved skill：會改變模型後續操作 guidance，應保持 task-aware validation。

目前所有策略共享類似 gate，造成低風險策略也被高成本 task outcome gate 卡住。

### P0-B：Task lifecycle 本身太重

`taskTools.ts` 仍存在多條直接 `captureBaseline(...)` 路徑，例如：

- task baseline check
- original baseline check
- expected state refresh
- full harness status
- change summary
- task start / finish flow

在大型 workspace，baseline entries 掃描成為 task lifecycle 的主成本。

這不只傷 latency，也會導致：

- response payload 過大
- workspace lock 等待放大
- task tool 錯誤後 recovery 成本變高
- task outcome 樣本不足
- learning validation 被間接阻塞

### P0-C：沒有 durable historical replay / backfill 閉環

`KnowledgeIngestor` 的主要設計是 runtime live-event queue。

雖然 KnowledgeStore 已使用 `sourceEventIds` 避免 evidence 重複計數，但缺少完整的：

- durable ingestion watermark
- compiler revision
- replay source cursor
- replay coverage
- recompile / re-materialize policy
- ignored reason distribution

結果是新增 classifier 或改善 learning rule 後，既有 31k+ 級 telemetry 不一定會自動重新轉化成新 knowledge。

### P0-D：History retrieval 仍以 recency 為主，不是 relevance retrieval

固定 recent window 在 session 少時合理，但 180 個 session 後會有顯著 recall loss。

典型失敗：

1. 半個月前決定過某個 protocol invariant。
2. 最近 12 個 session 都與別的專案細節相關。
3. 新 session bootstrap 看不到舊決策。
4. agent 重新推導甚至做出相反設計。

單純把 window 從 12 擴成 30 只會增加 token / payload，不會解決 relevance 問題。

### P1-A：Strategy implementation library 過窄

目前只有 1 個真正 intervention implementation。

即使 compiler 能辨識多種 pattern，沒有 executable implementation 就只能停留在觀察或 tool evolution proposal。

### P1-B：Retrieval / search policy 沒把已有 evidence 轉成 caller behavior

即使 bounded search 的成本已被觀測，caller 還是大量要求 exact total。

需要把「caller intent」和「是否真的需要 exact count」分開，而不是只靠 prompt guidance。

### P1-C：Polling 與 parallelism 的 evidence 收集不足

目前：

- `wait_command` 空等待比例高。
- parallelism telemetry 仍可出現 `observed_pairs = 0`，即使 `exec_many` 已有大量實際使用。

表示 learning 只接受太窄的 evidence 來源。

### P2：Observability 不能直接回答「為什麼沒學會」

現在可以看到 count，但很難一眼回答：

- 多少 event 進入 ingestor？
- 多少被 compiler 忽略？
- 忽略原因是什麼？
- 哪個 candidate 卡在 shadow？
- validation 缺的是 matched call、task outcome、verified rate 還是 failure rate？
- 有 implementation 但未 rollout，還是根本沒有 implementation？
- replay coverage 到哪個 diagnostics offset？

這使 learning pipeline 很難 operationally debug。

---

## 4. 目標架構

### 4.1 Learning pipeline

```text
canonical diagnostics
       |
       v
ExperienceNormalizer
       |
       +--> durable experience ledger (event-id dedupe)
       |
       v
ExperienceCompiler [compiler revision]
       |
       +--> ignored(reason)
       +--> tool_strategy
       +--> tool_evolution
       +--> evolved_skill
       |
       v
KnowledgeStore
       |
       v
ShadowEvaluator
       |
       v
ValidationGate
  | semantics-preserving
  | task-aware
  | evolution
       |
       v
CanaryStrategyEngine
       |
   control / intervention
       |
       v
Promote / Reject / Rollback
       |
       v
ImpactStore + Explainability snapshot
```

### 4.2 History retrieval pipeline

```text
history_session_checkpoint
       |
       v
structured history document
       |
       v
retrieval index
  - intent
  - findings
  - decisions
  - remaining issues
  - next actions
  - files changed
  - tests
  - title/session metadata
       |
       v
hybrid rank
  relevance + structured boosts + recency tie-break
       |
       v
recent N + relevant top-K
       |
       v
bounded restore context
```

---

## 5. 設計原則

### 5.1 Safety policy 永遠高於 learning

任何 learning strategy 都不得：

- 放寬 workspace boundary
- 自動繞過 confirmation
- 改寫 command allowlist
- 自動降低 protected repository policy
- 改變 secret handling
- 把 caller 明確要求的語意改成另一個語意

Knowledge / canary 只能在 static policy 允許的範圍內優化。

### 5.2 Validation 必須按 strategy safety class 分流

禁止用一組 threshold 套所有 strategy。

### 5.3 Replay 必須 idempotent

相同 `diagnostic_event_id` 不得重複計算 evidence。

### 5.4 Derived knowledge 必須可追溯

每個 knowledge record 應能回答：

- 由哪些 event 產生
- 由哪個 compiler revision 產生
- 使用哪個 implementation revision
- validation 為何 pass / block
- canary 為何 promote / rollback

### 5.5 History retrieval 優先 relevance，不用無限增加 recent window

recent window 只負責短期 continuity；舊知識靠 search/retrieval 找回。

### 5.6 所有新 optimization 都先 shadow，再 rollout

除純 observability / internal cache 且完全不影響語意的項目外，不直接全量切換。

### 5.7 TDD + 小批次

每一階段都應：

1. 先補 regression / characterization test。
2. 實作一個最小行為改變。
3. focused test。
4. full Node verify / 必要 Rust parity test。
5. benchmark。
6. commit。
7. 再做下一批。

---

# 6. Phase 0：先建立可信 baseline 與 blocker observability

## 6.1 目的

避免在 runtime/source mismatch 下優化錯誤目標，同時讓後續每個 learning record 的 blocker 可觀察。

## 6.2 工作項目

### 6.2.1 Runtime sync

- rebuild Node Agent from current HEAD。
- restart portable/server。
- `server_info.runtime_revision.matches_workspace === true` 才開始 benchmark。
- 記錄：
  - build SHA
  - workspace HEAD
  - server version
  - compiler revision
  - strategy implementation revision

### 6.2.2 Validation blocker explanation

擴充 `ValidationGateResult`，除了 aggregate count，增加 bounded blocker summary：

```ts
interface ValidationBlocker {
  knowledgeId: string;
  target: string;
  reason:
    | 'insufficient_shadow_matches'
    | 'insufficient_task_outcomes'
    | 'verified_task_rate_low'
    | 'task_failure_rate_high'
    | 'tool_failure_rate_high'
    | 'rollback_observed'
    | 'missing_implementation'
    | 'insufficient_control_samples'
    | 'insufficient_equivalence_samples';
  actual: number;
  required?: number;
}
```

對外 snapshot 只回 bounded top blockers，避免 response 膨脹。

### 6.2.3 Knowledge funnel metrics

新增：

```text
experience_seen
experience_normalized
experience_ignored
experience_compiled
knowledge_observed
knowledge_candidate
knowledge_shadow
knowledge_validated
knowledge_canary
knowledge_promoted
knowledge_rejected
knowledge_rolled_back
```

並增加 ignore reasons：

```text
unsupported_tool
pattern_not_matched
missing_event_id
workspace_mismatch
invalid_feature
missing_task_attribution
missing_skill_attribution
```

## 6.3 主要檔案

- `packages/node-agent/src/knowledge/validation.ts`
- `packages/node-agent/src/knowledge/ingestor.ts`
- `packages/node-agent/src/knowledge/compiler.ts`
- `packages/node-agent/src/managementObservability.ts`
- 對應 tests

## 6.4 驗收

- runtime/source SHA 一致。
- 任一 shadow record 可以直接看到「卡在哪個 gate」。
- `query_tool_usage` 或 management observability 能顯示完整 learning funnel。
- counters 不因 response payload 變大超過合理界線。

---

# 7. Phase 1：按 safety class 重構 Validation Gate

## 7.1 核心改動

在 strategy implementation spec 增加 validation class：

```ts
type StrategyValidationClass =
  | 'semantics_preserving'
  | 'task_aware'
  | 'behavioral';
```

例如：

```ts
{
  id: 'search_exact_total_fast_tail',
  safety: 'semantics_preserving',
  validationClass: 'semantics_preserving'
}
```

## 7.2 Semantics-preserving strategy gate

此類策略不應強制要求完整 task outcome。

建議最低 gate：

```text
shadow matched >= 20
AND tool-level failures <= baseline + tolerance
AND semantic equivalence failures == 0
AND verification failures == 0
AND enough independent contexts >= 2
```

如果策略是 performance optimization，再要求：

```text
candidate p50/p95 或 average latency 對 control 有正向改善
```

注意：validation 階段不要求一定達到 promotion 的 10% 性能門檻；promotion 才要求更強 evidence。

## 7.3 Semantic equivalence

對 `search_exact_total_fast_tail` 應加入明確 equivalence signals：

- returned matches 相同。
- exact total 相同。
- pagination cursor / truncation contract 相同。
- warnings / error contract 不增加。

如果 implementation 只改內部 scan tail，不改 output，這些可以做 deterministic comparison。

### Shadow 方法

最安全做法：

- control 正常執行。
- 在 bounded sampling 中以內部 shadow path 計算 candidate 結果。
- 不把 candidate response 回 caller。
- 比較 normalized response contract。
- 只記錄 hash / scalar difference，不記錄完整內容。

## 7.4 Task-aware strategy gate

以下仍維持 task outcome：

- 會改變 retry / recovery sequence 的策略。
- behavioral routing。
- evolved skill。
- 可能改變 caller-visible result set 的 optimization。

可以保留現有：

```text
MIN_TASK_OUTCOMES = 3
MIN_VERIFIED_TASK_RATE = 0.8
MAX_TASK_FAILURE_RATE = 0.2
```

但 blocker 要可見。

## 7.5 Promotion gate

promotion 保持比 validation 更嚴格：

- intervention calls >= 20
- control calls >= 10
- no rollback
- failure rate 不惡化
- 有足夠 independent contexts
- performance strategy 必須達明顯改善

對 semantics-preserving strategy：

- task outcome 可作額外 evidence，但不應成為唯一 mandatory evidence。

## 7.6 Rollback

任何：

- semantic mismatch
- error contract regression
- caller-visible count mismatch
- post-promotion failure spike

立即 reject / rollback。

## 7.7 主要檔案

- `packages/node-agent/src/knowledge/strategyImplementations.ts`
- `packages/node-agent/src/knowledge/validation.ts`
- `packages/node-agent/src/knowledge/shadow.ts`
- `packages/node-agent/src/knowledge/canary.ts`
- `packages/node-agent/src/knowledge/types.ts`
- tests

## 7.8 驗收

- `search_exact_total_fast_tail` 不再因缺少 `finish_task` 樣本永久卡住。
- 仍不能在 equivalence evidence 不足時 promotion。
- static security policy 不受影響。
- canary impact 開始有 control/intervention samples。

---

# 8. Phase 2：重做 Task baseline，使 outcome 收集變便宜

## 8.1 問題

目前 `captureBaseline(root)` 在大型 workspace 會掃描大量 entries，task lifecycle 的性能因此與 workspace 檔案總量高度相關。

但 task safety 真正需要回答的是：

1. Git identity 是否變了？
2. task 開始時的 dirty/untracked state 是否變了？
3. 已修改檔案是否在我們不知道的情況下被外部改寫？

不一定需要每次掃描整個 55k+ file workspace。

## 8.2 Baseline v2：Git workspace fast path

對 Git workspace，將 baseline 拆成：

```ts
interface ProjectBaselineV2 {
  branch: string;
  head: string;
  statusFingerprint: string;
  changedEntries: BaselineEntry[];
  untrackedEntries: BaselineEntry[];
  workspaceGeneration: number;
  capturedAt: number;
}
```

### 計算方式

1. `git branch/HEAD`：快速取得 identity。
2. `git status --porcelain=v1 -z --untracked-files=all`：取得 changed/untracked path set。
3. 只 hash changed/untracked files；不要 hash 所有 tracked clean files。
4. 由 path + file hash + mode 組成 `statusFingerprint`。

優點：

```text
O(all workspace files)
 -> O(git status + changed/untracked files)
```

對大 repo、小 diff 情境改善最大。

### 已經 dirty 的檔案再次被外部改寫

因 changed files 本身會 hash，所以仍可偵測。

### clean tracked file 被修改

下一次 `git status` 會進入 changed set，再納入 hash。

## 8.3 Non-Git fallback

非 Git workspace 可保留 baseline entries，但加入：

- directory stat cache
- per-path generation
- internal mutation invalidation
- bounded full rescan fallback

不得因 cache 讓 workspace boundary 或 stale edit protection 失效。

## 8.4 Response payload compacting

Task response 不應回傳完整 baseline entries。

預設只回：

```text
baseline_mode
git_identity
fingerprint
changed_file_count
untracked_file_count
baseline_capture_ms
baseline_cache_hit
```

完整 file detail 只在：

- explicit diagnostic mode
- bounded page request

才取回。

## 8.5 Change summary

`change_summary` 應優先使用 task operation records / changed path set，而不是每次重新 full capture。

需要外部 drift 檢查時：

- 只針對 task touched files + Git changed set 做 refresh。

## 8.6 主要檔案

- `packages/node-agent/src/taskTools.ts`
- `packages/node-agent/src/processes/harnessTracking.ts`
- `packages/node-agent/src/state.ts`
- `packages/node-agent/src/types.ts`
- tests

Rust parity：

- 對應 Harness / baseline implementation
- registry contract 只在 schema 有變化時同步

## 8.7 Performance gate

大型 fixture 驗收：

```text
50k files, <100 changed files
```

目標：

- `start_task` baseline p95 < 2 s。
- `finish_task` baseline p95 < 2 s。
- `change_summary` p95 < 2 s。
- default response < 128 KiB。
- baseline drift regression tests 全部通過。

如果 Windows / Dropbox 特殊檔案系統下達不到 2 秒，至少要求相較目前 100x 級改善，並把剩餘成本量化。

---

# 9. Phase 3：Durable ingestion ledger + replay/backfill

## 9.1 目標

讓新 compiler / classifier 可以重新利用既有 telemetry，而不是只等待新資料。

## 9.2 Experience ledger

增加 durable normalized event ledger，例如：

```text
<dataDir>/knowledge/<workspace>/experience.jsonl
```

每筆包含 bounded/sanitized：

```ts
interface ExperienceLedgerRecord {
  schemaVersion: 1;
  eventId: string;
  tool: string;
  outcome: 'success' | 'failure';
  durationMs: number;
  errorCode?: string;
  features?: Record<string, Scalar>;
  verificationOutcome?: 'passed' | 'failed';
  recoveryOutcome?: 'passed' | 'failed';
  taskIdHash?: string;
  conversationContextIdHash?: string;
  runtimeBootIdHash?: string;
  timestampMs: number;
}
```

不要存：

- 原始 prompt
- file contents
- secret
- full command output

## 9.3 Deduplication

主鍵：

```text
diagnostic_event_id
```

若 event 已存在，不重寫、不重算 live evidence。

## 9.4 Ingestion state

新增：

```json
{
  "schemaVersion": 1,
  "compilerRevision": "...",
  "normalizerRevision": "...",
  "lastCanonicalSourceRevision": "...",
  "lastReplayAtMs": 0,
  "ledgerEvents": 0,
  "materializedEvents": 0,
  "ignoredEvents": 0
}
```

## 9.5 Compiler revision

compiler 規則有任何會改變分類結果的變更時，revision 必須變。

例如由：

```text
hash(classifier source + feature schema version)
```

產生。

## 9.6 Re-materialization

不要直接把新分類 evidence 疊加到舊分類上造成 double count。

建議將 Knowledge 分成：

### Derived evidence layer

可由 ledger + compiler revision 重建：

- observed
- candidate
- shadow baseline evidence

### Lifecycle layer

需要保留：

- validated
- promoted
- rejected
- rolled back
- canary impact
- experiment result

當 compiler revision 改變：

1. 從 ledger 重新 materialize derived records。
2. canonical knowledge ID 相同者合併 provenance。
3. 舊 lifecycle record 若 no longer matched，不直接刪除；標 `staleCompilerRevision` / `superseded`。
4. promoted strategy 不因 compiler rebuild 自動重新啟用另一個 implementation。

## 9.7 Diagnostics backfill

新增 bounded admin/internal path：

```text
replay canonical diagnostics -> normalize -> ledger
```

要求：

- 支援 log rotation。
- event-id dedupe。
- 每批例如 500–2,000 events。
- 不阻塞主 tool call lane。
- 可 resume。
- 有 replay progress。

## 9.8 Replay metrics

```text
source_events_seen
source_events_deduped
ledger_events_added
ledger_events_invalid
materialized_records
ignored_by_reason
replay_cursor
replay_complete
compiler_revision
```

## 9.9 主要檔案

- `packages/node-agent/src/knowledge/ingestor.ts`
- `packages/node-agent/src/knowledge/compiler.ts`
- `packages/node-agent/src/knowledge/store.ts`
- 新增 `knowledge/ledger.ts`
- 新增 `knowledge/replay.ts`
- management observability
- tests

## 9.10 驗收

- 重啟後 ingestion lifetime state 不歸零成無法解讀的狀態。
- replay 同一批 diagnostics 兩次，evidence 不增加。
- compiler revision 改變後可 deterministic rebuild。
- 既有 31k+ 級 telemetry 可以被新 pattern backfill。

---

# 10. Phase 4：History relevance retrieval

## 10.1 不採用方案

不建議只做：

```text
HISTORY_SUMMARY_WINDOW 12 -> 30 / 50
```

原因：

- payload 線性增加。
- relevance 不一定提升。
- 越老但越關鍵的決策仍可能掉出 window。

## 10.2 Retrieval index

每次 `history_session_checkpoint` 成功後，更新一份 derived index。

索引欄位：

```ts
interface HistoryRetrievalDocument {
  sessionNumber: number;
  path: string;
  updatedAt: string;
  title: string;
  userIntent: string[];
  findings: string[];
  decisions: string[];
  remainingIssues: string[];
  nextActions: string[];
  filesChanged: string[];
  tests: string[];
  summary: string;
}
```

Index 是 derived artifact，損壞時必須可從 Markdown rebuild。

## 10.3 V1 ranking：dependency-free hybrid lexical

第一版不引入 embedding runtime dependency。

建議 score：

```text
score =
  term_score * 1.0
  + exact_phrase_boost
  + decision_field_boost
  + file_path_overlap_boost
  + unresolved_issue_boost
  + bounded_recency_tiebreak
```

中英文混合需要語言中立 tokenization：

- ASCII words / identifiers
- path tokens
- CJK character 2-gram / 3-gram
- exact quoted phrase

這比只用 whitespace token 更適合繁中歷史。

## 10.4 新工具：`history_search`

建議 schema：

```json
{
  "query": "string",
  "top_k": 8,
  "recent_boost": 0.15,
  "fields": ["intent", "findings", "decisions", "remaining_issues", "next_actions", "files", "tests"],
  "include_excerpt": true
}
```

限制：

- `top_k` default 8, max 20。
- excerpt bounded。
- 不回整個 session 文件。
- 回 session number/path/score/matched fields/excerpt。

## 10.5 Bootstrap 行為

bootstrap 改為：

```text
recent 3-5 session compact continuity
+ latest handoff
+ retrieval index metadata
```

不要自動塞 12 個長摘要。

可選新增：

```text
recall_query
```

若 caller 已知道本次 intent，可直接：

```text
recent N + relevant top-K
```

若沒有 recall query：

- bootstrap 回 `history_search_recommended=true`。
- assistant 根據 user intent 呼叫 `history_search`。

## 10.6 Rename ambiguous fields

`all_history_summary` 目前其實不是 all history。

相容期可保留舊欄位，但新增更準確名稱：

```text
recent_history_summary
recent_history_loaded_count
history_total_count
```

舊 `all_history_summary` 標記 deprecated，再逐步移除。

## 10.7 Retrieval usefulness telemetry

記錄：

```text
history_search_calls
history_search_top_k
history_search_selected_session_count
history_search_followup_read_count
history_search_zero_result
history_search_result_reused
```

若能從後續 action attribution 推導，再記：

```text
retrieved_session_contributed_to_task
```

但不要假裝能從單次 search 就知道「答案是否正確」。

## 10.8 主要檔案

- `packages/node-agent/src/history.ts`
- `packages/node-agent/src/historyStorage.ts`
- `packages/node-agent/src/historyMarkdown.ts`
- 新增 `historyRetrieval.ts`
- tool registry / generated catalog
- Rust parity history module
- tests

## 10.9 測試 fixture

至少建立：

### Case A：舊決策 relevance

- 100 個 sessions。
- 重要 decision 在 session 12。
- query 精確描述該 architecture invariant。
- session 12 必須進 top 3。

### Case B：recent noise

- 最近 20 個 session 有大量相似一般文字。
- 更老 session 含 exact file + decision。
- older exact decision 應高於 recent noise。

### Case C：繁中

- query：「全域歷史對話」
- session 使用「全域 resume / 歷史 session」相關文字。
- CJK n-gram 應能召回。

### Case D：index rebuild

- 刪 derived index。
- 從 Markdown rebuild。
- ranking deterministic。

---

# 11. Phase 5：把已知低效模式變成 executable strategies

## 11.1 Exact-total strategy

完成 Phase 1 後，優先讓：

```text
search_exact_total_fast_tail
```

真正走完整：

```text
shadow -> validated -> 25% canary -> promoted / rollback
```

## 11.2 Search intent normalization

新增 caller-intent classifier 不需要 LLM；可由 args 決定：

### 真正需要 exact count

- `count_only=true`
- caller explicit exact-total intent
- API contract 明確要求 total

### 只需要 bounded discovery

- max results + match previews
- 找檔案/符號/位置
- exploratory search

對後者 default `calculate_total=false`。

如果 caller 明確傳 `calculate_total=true`，不得私自改成 false；只能透過 semantics-preserving exact-total implementation 優化內部算法。

## 11.3 `apply_patch` strategy

根據目前高失敗率，加入 strategy / recommendation：

```text
if patch is small deterministic file edit
and guarded edit can express the mutation
prefer `edit`
```

不是把 `apply_patch` 移除；以下仍適合 patch：

- 多 hunk diff 已知完整 context。
- caller 明確提供 patch。
- edit operation 無法簡潔表示。

應記：

```text
patch_fallback_to_edit_recommended
patch_failure_recovered_by_edit
```

## 11.4 Stale edit recovery

既有 `FILE_VERSION_MISMATCH` / `EDIT_MATCH_COUNT_MISMATCH` pattern 已可辨識。

下一步：

- direct recovery action 優先。
- 重新讀取 bounded current target。
- 重建 guard。
- 禁止 unchanged retry。

這類 strategy 仍屬 task-aware / behavioral，不使用 semantics-preserving fast gate。

---

# 12. Phase 6：Adaptive wait 與 parallel evidence

## 12.1 Adaptive `wait_command`

325/970 empty timeout 表示固定 polling 並不理想。

每個 retained session 增加 runtime hints：

```text
last_output_at
observed_output_interval
process_kind
elapsed_ms
recent_cpu/output activity (若安全可取得)
```

回傳：

```text
recommended_wait_ms
recommended_until
```

範例：

- 高輸出 command：2–5 s。
- quiet build/test：15–20 s。
- 已長時間無輸出但 process healthy：20 s transport cap + continue reattach。

不要讓 recommended wait 延長 child deadline。

## 12.2 Empty-timeout feedback

記錄：

```text
wait_recommendation_used
wait_recommendation_delta_ms
wait_returned_empty
wait_returned_output
wait_returned_terminal
```

讓策略可用 evidence 調整。

## 12.3 Parallelism evidence cold-start

目前 pair model 不應只從 explicit parallel run 學習。

可收集：

- explicit parallel success/failure：高權重。
- resource lock serialization：明確 conflict evidence。
- overlapping filesystem write scopes：conflict evidence。
- independent read-only command pairs：shadow-safe evidence。
- sequential pairs 不當成「已證明可 parallel」，但可作 candidate discovery。

保留原則：

```text
unknown pair 不因弱 evidence 自動 parallel
```

只改善 candidate generation，不降低安全門檻。

---

# 13. Phase 7：Learning observability / explanation surface

## 13.1 Snapshot

新增 bounded learning snapshot：

```json
{
  "experience": {
    "ledger_events": 0,
    "replay_complete": false,
    "compiler_revision": "..."
  },
  "funnel": {
    "observed": 0,
    "candidate": 0,
    "shadow": 0,
    "validated": 0,
    "promoted": 0,
    "rejected": 0
  },
  "top_blockers": [],
  "strategies": []
}
```

## 13.2 Per-strategy explain

每個 strategy 應可看到：

```text
id
target
status
implementation
validation class
observations
independent contexts
shadow matches
validation blocker
control calls
intervention calls
failure rates
avg/p50/p95 duration
semantic mismatch count
promoted_at / rollback_at
rollback_reason
```

## 13.3 Lifetime vs runtime counters

明確分開：

```text
runtime_ingestion.*
durable_lifetime.*
```

避免重啟後看到 processed=0 就誤以為 knowledge store 沒資料。

---

# 14. 建議實作批次

遵照小批次 TDD，建議拆成以下 commits。

## Batch A：Blocker observability

內容：

- validation blocker reason
- learning funnel
- lifetime/runtime counter distinction
- tests

不改 rollout behavior。

### Done

可以回答「為什麼 3 個 strategy 都沒 validated」。

---

## Batch B：Semantics-preserving validation class

內容：

- implementation validation class
- equivalence evidence type
- new validation path
- 保留 task-aware old path
- tests

### Done

`search_exact_total_fast_tail` 可以不靠 3 個 task outcome 進 validated，但必須有足夠 equivalence evidence。

---

## Batch C：Search canary closure

內容：

- shadow equivalence sampling
- validated -> deterministic 25% canary
- control/intervention metrics
- promotion / rollback test

### Done

第一個真實 strategy 完成完整 lifecycle。

---

## Batch D：Baseline v2

內容：

- Git fast baseline
- changed/untracked-only hashes
- task response compacting
- characterization + performance fixture

### Done

50k file fixture 不再產生分鐘級 task lifecycle latency。

---

## Batch E：Experience ledger / replay

內容：

- normalized durable ledger
- event dedupe
- compiler revision
- replay progress
- deterministic rebuild tests

### Done

新 classifier 可以吃既有 diagnostics。

---

## Batch F：History retrieval index

內容：

- derived history index
- CJK/token/path tokenizer
- ranking
- index rebuild
- tests

### Done

能從 100+ session 中召回老的關鍵 decision。

---

## Batch G：`history_search` tool

內容：

- Node tool
- schema
- bounded excerpts
- generated catalog
- Rust parity
- bootstrap hints

### Done

agent 不需要把 history files 當普通 workspace search 來找。

---

## Batch H：Search/apply_patch/wait strategies

內容：

- caller intent metrics
- apply_patch -> edit recommendation
- adaptive wait recommendation
- corresponding compiler patterns

### Done

有第二、第三類 strategy 進入 shadow/canary，而不是只有 exact-total 一條。

---

# 15. Testing strategy

## 15.1 Unit tests

### Knowledge

- record ID stability
- replay dedupe
- compiler revision change
- semantics-preserving validation
- task-aware validation
- blocker reason
- promotion threshold
- rollback

### History

- tokenizer
- field boost
- recency tie-break
- deterministic ordering
- index rebuild
- bounded response
- CJK recall

### Baseline

- clean Git repo
- dirty tracked file
- untracked file
- already-dirty file changes again
- branch/HEAD change
- external edit during task
- non-Git fallback

## 15.2 Integration tests

### Search strategy lifecycle

1. 建立 candidate。
2. shadow 20+ calls。
3. semantic equivalence 100%。
4. validated。
5. control + intervention samples。
6. latency improvement。
7. promoted。
8. 注入 regression。
9. automatic rollback。

### Replay

1. 建一批 diagnostics。
2. ingest。
3. restart。
4. replay。
5. evidence 不 double-count。
6. bump compiler revision。
7. deterministic re-materialize。

### History

- 180-session fixture。
- recent irrelevant / old relevant。
- `history_search` 找到 old relevant。
- bootstrap response bytes 有上限。

## 15.3 Full verification

每個影響 Node Agent runtime 的 batch：

```text
pnpm --filter @coding-tools/node-agent run verify
```

影響 shared registry / Rust parity 時再跑對應 Rust tests。

避免 Node build 與同一 dist tree 的 Node tests 平行執行，以免重現 dist cleanup race。

---

# 16. Benchmark 設計

## 16.1 固定 benchmark fixture

建立 deterministic fixture：

```text
small repo: 1k files
medium repo: 10k files
large repo: 50k files
```

每種再有：

```text
clean
10 changed
100 changed
1000 changed
```

## 16.2 測量項目

### Task

- start_task p50/p95
- finish_task p50/p95
- change_summary p50/p95
- baseline capture ms
- response bytes

### Search

- bounded search p50/p95
- exact total p50/p95
- files scanned
- output equivalence

### History

- bootstrap response bytes
- bootstrap p95
- history_search p95
- top-k recall fixture

### Learning

- event -> candidate latency
- candidate -> shadow samples
- shadow -> validated latency
- validated -> canary samples
- canary -> promoted samples

## 16.3 實測結果（2026-09-16）

以下數字是在 Windows + Dropbox 工作目錄上以獨立暫存 fixture 測得。`.tmp/*.json` 原始輸出不納入版本控制；benchmark runner 已納入版本控制，可重現量測。

### Task 實測

`start_task` / `change_summary` / `finish_task` 已用 1k、10k 與 50k repo 驗證。50k fixture 因建立與掃描成本高，採 3 iterations；0 / 10 / 100 / 1000 changed 均完成。

- 50k / clean：start p50/p95 = 474.90 / 969.11 ms；summary = 403.87 / 1542.80 ms；finish = 500.65 / 1534.14 ms。
- 50k / 10 changed：start = 1184.19 / 1584.25 ms；summary = 434.52 / 1562.05 ms；finish = 433.14 / 1757.61 ms。
- 50k / 100 changed：start = 1808.99 / 2008.34 ms；summary = 463.52 / 1527.01 ms；finish = 1703.75 / 1861.29 ms。
- 50k / 1000 changed：start = 1204.45 / 3437.59 ms；summary = 1215.64 / 2028.41 ms；finish = 1810.70 / 3850.87 ms。
- 1000 changed 時 start/finish response 仍約 41–43 KiB；完整 baseline 保留在 durable state，只對 wire response 做 256-entry projection。
- `BASELINE_READ_CONCURRENCY=32` 的 A/B 使 Windows tail latency 變差，因此維持 16。
- 結論：response size gate 明確通過；`baseline p95 < 2 s` 是 preferred 目標，在 clean / 10 / 多數 100 changed 情境接近或達成，但 1000 dirty files 的 Windows/Dropbox tail latency 仍未穩定達成。

### Search 實測

正式 5-iteration 結果：

- 1k：bounded p50/p95 = 1503.25 / 19330.57 ms；exact-total = 5641.36 / 33541.97 ms。
- 10k：bounded p50/p95 = 1797.18 / 24637.59 ms；exact-total = 11856.12 / 36456.43 ms。
- bounded search 在兩種 corpus 都固定掃 101 files；exact-total 分別掃完整 1000 / 10000 files。
- bounded 與 exact-total 的前 100 筆結果在所有正式 run 中完全等價。
- p95 有明顯 Windows/Dropbox I/O tail variance，但掃描量顯示 bounded discovery 的成本不會隨 corpus 線性擴張；exact-total 則會完整掃描 corpus。
- 50k exact-total 未做 5-iteration 正式跑，因 1k/10k 已證明線性 full-scan 特性；若需要完整矩陣，只補 50k 單次或少量 sample 即可。

### History 實測

180-session fixture、20 個分散於舊 session 的人工 relevant-decision queries：

- `history_search`：p50 = 45.35 ms，p95 = 68.69 ms，response = 2400 bytes。
- top-5 recall：20/20 = 100%，全部 rank 1。
- compact bootstrap：p50 = 54.49 ms，p95 = 138.07 ms，response 約 17.6 KiB。
- derived retrieval index 正常使用；180-session index rebuild 約 64.12 ms。
- 結論：top-5 recall >= 95% 與 bounded bootstrap payload 均通過。

### Learning 實測

使用真正的 `KnowledgeIngestor`、`KnowledgeStore`、shadow/canary impact stores，正式跑 5 次完整 lifecycle：

- event -> candidate/shadow：p50 = 36.69 ms，p95 = 266.19 ms。
- candidate -> shadow：固定 5 evidence samples；candidate 在 ingestion flush 內立即自動進 shadow，因此 wall-clock 與 event->candidate 同批量測。
- shadow -> validated：固定 20 shadow samples；p50 = 315.53 ms，p95 = 2919.28 ms。
- validated -> canary：固定 1 次 decision 即進入 canary eligibility。
- canary -> promoted：固定 30 samples（10 control + 20 intervention）；p50 = 585.66 ms，p95 = 4605.93 ms。
- 五次 run 均為 20 semantic-equivalence checks、0 failures、2 held-out contexts，最後均成功 promoted。

## 16.4 Benchmark runner

可重現指令入口：

```text
pnpm --filter @coding-tools/node-agent benchmark:reflection-learning
pnpm --filter @coding-tools/node-agent benchmark:reflection-learning:search
pnpm --filter @coding-tools/node-agent benchmark:reflection-learning:history
pnpm --filter @coding-tools/node-agent benchmark:reflection-learning:learning
```

---

# 17. Success metrics

## P0 success

### Learning closure

目標：

```text
validated_tool_strategy_count > 0
canary_impact_count > 0
```

並至少有一個 semantics-preserving strategy 完成 promotion 或有明確可解釋 rejection。

### Task lifecycle

大型 repo：

```text
baseline p95 < 2 s preferred
response < 128 KiB default
```

### Replay

```text
replay duplicate evidence = 0
compiler revision coverage = 100% of retained canonical events that satisfy retention window
```

### History retrieval

人工 fixture：

```text
old relevant decision top-5 recall >= 95%
```

不是用最近 session 當答案，而是用 relevance fixture 驗證。

## 17.1 2026-09-16 P0 gate 狀態

| Gate | 狀態 | 實測證據 |
|---|---|---|
| `validated_tool_strategy_count > 0` | PASS | 真實 learning lifecycle 五次皆由 shadow 進 validated，之後進 canary / promoted。 |
| `canary_impact_count > 0` | PASS | 五次皆產生 canary impact，固定 10 control + 20 intervention 後 promotion。 |
| semantics-preserving strategy promotion / explainable rejection | PASS | `search_exact_total_fast_tail` 五次皆完成 promotion；另有既有 rollback regression 驗證 promoted 後可安全 rejected。 |
| Task response `< 128 KiB` | PASS | 1000 changed 的 start/finish 約 41–43 KiB。 |
| Task baseline p95 `< 2 s preferred` | PARTIAL | clean / 10 changed 與多數 100 changed 接近或達標；1000 dirty files 在 Windows/Dropbox 測得約 2.0–3.9 s tail。這是 preferred，不視為 blocking correctness gate。 |
| replay duplicate evidence = 0 | PASS | replay/rematerialization regression 已驗證 durable ledger dedupe；revision 變更會重新 materialize，不重複 evidence。 |
| compiler revision retained-event coverage | PASS | revision-aware replay regression 驗證 retained canonical events 會依 compiler/normalizer revision 重新覆蓋。 |
| old relevant decision top-5 recall `>= 95%` | PASS | 180-session / 20-query fixture = 20/20，100%，全部 rank 1。 |

P0 correctness / learning-closure gate 已成立；剩餘性能工作主要是 large dirty-set 的 Windows tail latency，以及 P1 的 production behavior 指標，不應用放寬 correctness gate 交換速度。

## P1 success

### Search

exact-total 不應再因 caller default /不必要設定維持 90%+。

注意：如果使用者或上層 caller 真正要求 exact total，比例高本身不是錯；應把「exact-total required by intent」和「accidental/default exact-total」拆開量。

### Wait

```text
empty wait timeout rate: 33.5% -> < 15%
```

### Apply patch

```text
preventable apply_patch failure 明顯下降
```

## 17.2 P1 production baseline 與新版量測欄位（2026-09-16）

目前 production Node Agent `0.29.35` 的歷史 telemetry 尚未達 P1，且部分舊欄位不足以直接表示 caller intent：

- Search：`search_text` 771 calls；舊 `exact_total_calls=709`（約 92.0%）。但此欄位表示「結果最後是 exact」，不是「caller 明確要求 exact」，因此不能直接拿 92.0% 判定 caller 行為。
- Wait：`wait_command` 1211 calls；舊 `empty_wait_timeouts=414`（約 34.2%），仍高於 `<15%` 目標。但其中包含 20 s transport-safe cap 到期後必須 reattach 的合法情境。
- Apply patch：23 calls / 19 errors（約 82.6%）；主要 error 為 `PATCH_CHECK_FAILED=14`、`PATCH_PREFLIGHT_FAILED=3`、`PATCH_CONTEXT_NOT_FOUND=2`（另有其他非主要錯誤）。

本 feature branch 已新增可用於 after-window 的精確分類：

### Search caller intent

```text
search.caller_intent_observed_calls
search.calculate_total_explicit_calls
search.calculate_total_requested_calls
search.count_only_requested_calls
search.calculate_total_omitted_calls
```

保留既有 `exact_total_calls` 作為結果 exactness 相容欄位；不得再把它解讀為 caller intent。

### Wait feedback / avoidable empty polling

`wait_command` 不改變當次 requested wait，也不延長 child process deadline；只調整回傳的下一步 recommendation。新增：

```text
recommended_wait_ms
recommended_until
since_last_output_ms
wait_returned_empty
wait_returned_output
wait_returned_terminal
wait_transport_cap_reached
```

短空 poll 會 bounded backoff；build/test 類 quiet command 可直接建議 20 s transport-safe window。Telemetry 同時保留總 `empty_wait_timeouts`，並新增：

```text
avoidable_empty_wait_timeouts
transport_cap_empty_waits
wait_returned_output_calls
wait_returned_terminal_calls
wait_recommendation_used_calls
```

因此 P1 `<15%` 的主要 after 指標應改看 **avoidable empty wait timeout rate**；transport-cap reattach 不應被當成 polling 策略失敗。

### Apply-patch preventable funnel

新增：

```text
patch.check_failures
patch.preflight_failures
patch.context_failures
patch.edit_recommended_failures
patch.preventable_simple_failures
```

`preventable_simple_failures` 僅計入「單檔、最多一個 hunk，而且失敗時工具已明確推薦 `edit`」的 apply-patch error；caller 明確提供 patch 時 server 不會靜默改寫成 edit。部署新版後應比較此欄位的 call-normalized rate，而不是只看所有 apply-patch error rate。

### After-window 判定

P1 不可用 feature branch 的單元測試假裝已改善 production。完成 rollout 後至少收集同一版本的一個穩定 usage window，再比較：

1. `calculate_total_requested_calls / caller_intent_observed_calls` 與 omitted/default 分布。
2. `avoidable_empty_wait_timeouts / wait_command calls` 是否 `<15%`，並另列 transport-cap reattach。
3. `preventable_simple_failures / apply_patch calls` 是否較上述 baseline 顯著下降。

---

# 18. Rollout 與風險

## 18.1 Validation gate 改太鬆

風險：低樣本 strategy 被過早 validated。

防護：

- semantics-preserving 必須有 semantic equivalence evidence。
- validation 只進 25% canary，不直接 promoted。
- promotion threshold 維持更嚴格。
- regression automatic rollback。

## 18.2 Replay 造成 double count

防護：

- event ID ledger dedupe。
- derived materialization 與 lifecycle 分層。
- compiler revision provenance。
- replay idempotency test。

## 18.3 Baseline cache 漏掉 external edits

防護：

- Git status + changed/untracked file hash。
- 已 dirty file 每次 check 重新 hash。
- branch/HEAD identity 獨立檢查。
- non-Git fallback 保守。
- security-sensitive write preflight 不只依賴 stale cache。

## 18.4 History index 損壞

防護：

- index 是 derived artifact。
- Markdown 是 source of truth。
- validate/rebuild command。
- atomic temp-write + rename。

## 18.5 Retrieval 噪音

防護：

- recent continuity 與 relevance retrieval 分開。
- score breakdown 可觀察。
- deterministic top-k。
- top-k bounded。

---

# 19. 暫不優先項目

以下目前不應搶 P0 資源：

## 19.1 Generic error taxonomy 再大改

近期 generic `TOOL_FAILED` 已大幅下降，error classification 相較早期成熟很多。

仍可維護，但不是目前 learning 閉環的主要 blocker。

## 19.2 直接導入 ML / contextual bandit

現在連 deterministic evidence funnel 都還沒有完整打通。

先完成：

```text
stable signatures
explicit labels
replayable evidence
validated strategy lifecycle
```

之後才評估 contextual bandit。

## 19.3 History embedding 第一版

180 session 規模用 structured lexical + CJK n-gram 已足以先驗證 retrieval architecture。

embedding 可作 Phase 2 enhancement，而不是阻塞第一版。

---

# 20. 預計程式落點

## Knowledge / learning

- `packages/node-agent/src/knowledge/ingestor.ts`
- `packages/node-agent/src/knowledge/compiler.ts`
- `packages/node-agent/src/knowledge/validation.ts`
- `packages/node-agent/src/knowledge/shadow.ts`
- `packages/node-agent/src/knowledge/canary.ts`
- `packages/node-agent/src/knowledge/store.ts`
- `packages/node-agent/src/knowledge/impact.ts`
- `packages/node-agent/src/knowledge/strategyImplementations.ts`
- `packages/node-agent/src/knowledge/types.ts`
- new `packages/node-agent/src/knowledge/ledger.ts`
- new `packages/node-agent/src/knowledge/replay.ts`

## Task baseline

- `packages/node-agent/src/taskTools.ts`
- `packages/node-agent/src/processes/harnessTracking.ts`
- `packages/node-agent/src/state.ts`
- `packages/node-agent/src/types.ts`

## History

- `packages/node-agent/src/history.ts`
- `packages/node-agent/src/historyStorage.ts`
- `packages/node-agent/src/historyMarkdown.ts`
- new `packages/node-agent/src/historyRetrieval.ts`

## Observability / public contract

- `packages/node-agent/src/managementObservability.ts`
- tool registry/schema
- `packages/node-agent/src/rustCatalog.generated.ts`（由 source contract 生成，不手工漂移）
- Rust registry/history parity implementation

---

# 21. Definition of Done

這個計畫不能只以「程式碼寫完」判定完成。

完整 Done 必須同時滿足：

1. **Runtime trusted**
   running build 與 workspace HEAD 一致。

2. **Learning funnel 可解釋**
   可以知道每個 strategy 為何 observed / shadow / blocked / validated / rejected。

3. **至少一個真實 strategy 走完 lifecycle**
   `search_exact_total_fast_tail` 至少完成 validated + canary，最後 promotion 或 evidence-based rejection。

4. **Task lifecycle 不再是分鐘級**
   大 repo benchmark 有明確改善，且 safety regression 全過。

5. **Historical replay 可重入**
   新 compiler 可以利用舊 telemetry，同一資料 replay 不 double count。

6. **History relevance retrieval 可用**
   100+ session 下可以找回老但相關 decision，而不是只看最近 N 個。

7. **Payload bounded**
   task/history/learning observability 不重新產生 MB 級預設 response。

8. **Node / Rust contract 不漂移**
   公開 tool schema 與 shared behavior 有 parity test 或明確平台例外。

9. **Full verify 通過**
   Node verify、必要 Rust tests、diff check 全部通過。

10. **Before/after metrics 有紀錄**
    至少保留：search exact-total、task baseline、empty wait、learning funnel、history retrieval recall 五組 before/after 數據。

---

# 22. 建議實作順序

最終建議順序：

```text
0. rebuild/restart trusted runtime + baseline snapshot
1. validation blocker observability
2. semantics-preserving validation class
3. search strategy shadow equivalence + canary closure
4. task baseline v2 / compact responses
5. durable experience ledger + replay/backfill
6. history retrieval index
7. history_search + bootstrap recent/relevance split
8. apply_patch/edit + adaptive wait strategies
9. parallel evidence candidate collection
10. Node/Rust parity + final benchmark + docs update
```

這個順序的理由是：

- 先讓第一個已存在的 strategy 真正走通，可以驗證 learning 架構，而不是一次重寫所有系統。
- 接著處理 task baseline，解除 task outcome 資料收集的結構性成本。
- 再做 replay，才能讓新規則吃回既有大量資料。
- history retrieval 與 tool learning 是不同資料面，等前面 pipeline 可觀察後再加入，降低同時變更多個核心 persistence path 的風險。

---

## 23. 第一批實作建議

第一批只做三件事：

1. **Validation blocker explanation**
   不改策略行為，先讓 shadow 卡點可見。

2. **Semantics-preserving validation path**
   為 `search_exact_total_fast_tail` 補 equivalence evidence 與不依賴 task outcome 的 validation。

3. **真實 canary closure**
   讓這個 strategy 開始產生 control/intervention impact。

完成後再重新看數據。如果第一個 strategy 仍無法進 canary，先修 learning pipeline，不應急著擴充更多 classifier。
