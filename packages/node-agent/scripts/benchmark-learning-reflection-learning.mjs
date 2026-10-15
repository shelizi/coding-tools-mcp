import { mkdir, mkdtemp, rename, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';

const {
  CanaryImpactStore,
  CanaryStrategyEngine,
  KnowledgeIngestor,
  KnowledgeImpactStore,
  KnowledgeStore
} = await import('../dist/knowledge/index.js');

function progress(message) {
  process.stderr.write(`[learning-benchmark] ${message}\n`);
}

function integerArg(name, fallback, minimum = 1) {
  const prefix = `--${name}=`;
  const raw = process.argv.find(arg => arg.startsWith(prefix))?.slice(prefix.length);
  if (raw === undefined) return fallback;
  const value = Number.parseInt(raw, 10);
  if (!Number.isFinite(value) || value < minimum) throw new Error(`--${name} must be >= ${minimum}.`);
  return value;
}

function stringArg(name) {
  const prefix = `--${name}=`;
  return process.argv.find(arg => arg.startsWith(prefix))?.slice(prefix.length)?.trim() || undefined;
}

function percentile(values, fraction) {
  if (!values.length) return null;
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.max(0, Math.ceil(sorted.length * fraction) - 1)];
}

function stats(values) {
  if (!values.length) return { samples: 0, avg_ms: null, p50_ms: null, p95_ms: null, min_ms: null, max_ms: null };
  return {
    samples: values.length,
    avg_ms: Number((values.reduce((sum, value) => sum + value, 0) / values.length).toFixed(2)),
    p50_ms: Number(percentile(values, 0.5).toFixed(2)),
    p95_ms: Number(percentile(values, 0.95).toFixed(2)),
    min_ms: Number(Math.min(...values).toFixed(2)),
    max_ms: Number(Math.max(...values).toFixed(2))
  };
}

async function timed(operation) {
  const started = performance.now();
  const value = await operation();
  return { value, elapsedMs: performance.now() - started };
}

async function persistResult(outputPath, value) {
  if (!outputPath) return;
  const resolved = path.resolve(outputPath);
  await mkdir(path.dirname(resolved), { recursive: true });
  const temp = `${resolved}.${process.pid}.tmp`;
  await writeFile(temp, `${JSON.stringify(value, null, 2)}\n`, 'utf8');
  await rename(temp, resolved);
}

function trainingRecord(run, index) {
  return {
    event_type: 'tool_call',
    diagnostic_event_id: `learning-benchmark-${run}-train-${index}`,
    selected_workspace_id: 'repo',
    tool: 'search_text',
    outcome: 'success',
    duration_ms: 2_000,
    completed_ts_ms: 1_000 + index,
    calculate_total: true,
    files_considered: 1_000
  };
}

function shadowRecord(run, index) {
  return {
    event_type: 'tool_call',
    diagnostic_event_id: `learning-benchmark-${run}-shadow-${index}`,
    selected_workspace_id: 'repo',
    tool: 'search_text',
    outcome: 'success',
    duration_ms: 1_000,
    completed_ts_ms: 2_000 + index,
    calculate_total: true,
    files_considered: 1_000,
    knowledge_shadow_equivalence_implementation: 'search_exact_total_fast_tail',
    knowledge_shadow_equivalence_passed: true
  };
}

function canaryRecord(run, strategyId, side, index) {
  const intervention = side === 'intervention';
  return {
    event_type: 'tool_call',
    diagnostic_event_id: `learning-benchmark-${run}-${side}-${index}`,
    selected_workspace_id: 'repo',
    tool: 'search_text',
    outcome: 'success',
    duration_ms: intervention ? 600 : 1_000,
    completed_ts_ms: (intervention ? 4_000 : 3_000) + index,
    calculate_total: true,
    files_considered: 1_000,
    knowledge_canary_id: strategyId,
    knowledge_canary_implementation: 'search_exact_total_fast_tail',
    knowledge_canary_eligible: true,
    knowledge_canary_applied: intervention,
    knowledge_canary_bucket: intervention ? index : 50 + index
  };
}

async function lifecycleRun(run) {
  const root = await mkdtemp(path.join(tmpdir(), `ctmcp-learning-benchmark-${run}-`));
  try {
    const store = new KnowledgeStore(root);
    const shadowImpactStore = new KnowledgeImpactStore(root);
    const canaryImpactStore = new CanaryImpactStore(root);
    const strategyEngine = new CanaryStrategyEngine(store, canaryImpactStore);
    const ingestor = new KnowledgeIngestor(store, shadowImpactStore, 'repo', 1024, {
      impactStore: canaryImpactStore,
      strategyEngine
    });

    const trainingSamples = 5;
    const training = await timed(async () => {
      for (let index = 0; index < trainingSamples; index += 1) {
        ingestor.enqueueToolUsage(trainingRecord(run, index), { conversationContextId: `train-${index % 2}` });
      }
      await ingestor.flush();
    });
    let [strategy] = (await store.list()).filter(item => item.target === 'tool_strategy');
    if (!strategy) throw new Error('training did not create a tool strategy');
    if (strategy.status !== 'shadow') throw new Error(`expected auto-activated shadow strategy, got ${strategy.status}`);
    if (strategy.recommendedAction?.semantic_preserving_implementation !== 'search_exact_total_fast_tail') {
      throw new Error(`unexpected strategy implementation: ${JSON.stringify(strategy.recommendedAction)}`);
    }
    const candidateEvidenceSamples = Number(strategy.evidence?.observations ?? 0);

    let shadowSamples = 0;
    const shadowStarted = performance.now();
    while (shadowSamples < 40) {
      ingestor.enqueueToolUsage(
        shadowRecord(run, shadowSamples),
        { conversationContextId: `held-out-${shadowSamples % 2}` }
      );
      shadowSamples += 1;
      await ingestor.flush();
      strategy = await store.get(strategy.id);
      if (strategy?.status === 'validated') break;
    }
    const shadowToValidatedMs = performance.now() - shadowStarted;
    if (strategy?.status !== 'validated') throw new Error(`strategy did not validate after ${shadowSamples} shadow samples`);
    const [shadowImpact] = await shadowImpactStore.list();

    await strategyEngine.refresh();
    let canaryDecisionSamples = 0;
    let firstCanaryDecision = null;
    for (let index = 0; index < 100; index += 1) {
      canaryDecisionSamples += 1;
      const decision = await strategyEngine.decide('search_text', { query: 'needle', calculate_total: true }, `learning-${run}-${index}`);
      if (decision?.stage === 'canary') {
        firstCanaryDecision = decision;
        break;
      }
    }
    if (!firstCanaryDecision) throw new Error('validated strategy never became canary-eligible');

    let controlSamples = 0;
    let interventionSamples = 0;
    const canaryStarted = performance.now();
    while (controlSamples + interventionSamples < 100) {
      const side = controlSamples < 10 ? 'control' : 'intervention';
      const index = side === 'control' ? controlSamples : interventionSamples;
      ingestor.enqueueToolUsage(canaryRecord(run, strategy.id, side, index));
      if (side === 'control') controlSamples += 1;
      else interventionSamples += 1;
      await ingestor.flush();
      strategy = await store.get(strategy.id);
      if (strategy?.status === 'promoted') break;
    }
    const canaryToPromotedMs = performance.now() - canaryStarted;
    if (strategy?.status !== 'promoted') {
      throw new Error(`strategy did not promote after control=${controlSamples} intervention=${interventionSamples}`);
    }
    const [canaryImpact] = await canaryImpactStore.list();
    const snapshot = ingestor.snapshot();

    return {
      event_to_candidate_latency_ms: Number(training.elapsedMs.toFixed(2)),
      candidate_to_shadow_samples: candidateEvidenceSamples,
      shadow_to_validated_latency_ms: Number(shadowToValidatedMs.toFixed(2)),
      shadow_to_validated_samples: shadowSamples,
      validated_to_canary_samples: canaryDecisionSamples,
      canary_to_promoted_latency_ms: Number(canaryToPromotedMs.toFixed(2)),
      canary_to_promoted_samples: controlSamples + interventionSamples,
      canary_control_samples: controlSamples,
      canary_intervention_samples: interventionSamples,
      shadow_impact: {
        matched: shadowImpact?.matched ?? 0,
        semantic_equivalence_checks: shadowImpact?.semanticEquivalenceChecks ?? 0,
        semantic_equivalence_failures: shadowImpact?.semanticEquivalenceFailures ?? 0,
        context_count: shadowImpact?.matchedConversationContextIds?.length ?? 0
      },
      canary_impact: {
        control_calls: canaryImpact?.controlCalls ?? 0,
        intervention_calls: canaryImpact?.interventionCalls ?? 0,
        promoted: Boolean(canaryImpact?.promotedAtMs)
      },
      ingestion_snapshot: snapshot
    };
  } finally {
    await rm(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  }
}

const iterations = integerArg('iterations', 5);
const outputPath = stringArg('output');
const runs = [];
for (let run = 0; run < iterations; run += 1) {
  progress(`lifecycle run ${run}`);
  runs.push(await lifecycleRun(run));
}

const result = {
  schema_version: 1,
  benchmark: 'reflection-retrieval-learning',
  section: 'learning-lifecycle',
  generated_at: new Date().toISOString(),
  iterations,
  metrics: {
    event_to_candidate_latency: stats(runs.map(run => run.event_to_candidate_latency_ms)),
    candidate_to_shadow_samples: runs.map(run => run.candidate_to_shadow_samples),
    shadow_to_validated_latency: stats(runs.map(run => run.shadow_to_validated_latency_ms)),
    shadow_to_validated_samples: runs.map(run => run.shadow_to_validated_samples),
    validated_to_canary_samples: runs.map(run => run.validated_to_canary_samples),
    canary_to_promoted_latency: stats(runs.map(run => run.canary_to_promoted_latency_ms)),
    canary_to_promoted_samples: runs.map(run => run.canary_to_promoted_samples),
    canary_control_samples: runs.map(run => run.canary_control_samples),
    canary_intervention_samples: runs.map(run => run.canary_intervention_samples)
  },
  runs
};
await persistResult(outputPath, result);
process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
