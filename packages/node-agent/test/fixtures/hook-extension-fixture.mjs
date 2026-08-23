import process from 'node:process';
import { appendFile } from 'node:fs/promises';

const chunks = [];
for await (const chunk of process.stdin) chunks.push(chunk);
const input = JSON.parse(Buffer.concat(chunks.map(chunk => Buffer.from(chunk))).toString('utf8'));
const mode = process.argv[2] ?? 'rewrite';
if (mode === 'record') {
  const target = process.argv[3];
  if (!target) throw new Error('record target is required');
  await appendFile(target, `${JSON.stringify({
    event: input.hook_event_name,
    sessionId: input.session_id,
    cwd: input.cwd,
    source: input.source
  })}\n`);
  process.stdout.write('{}');
  process.exit(0);
}
if (mode === 'block') {
  process.stdout.write(JSON.stringify({
    hookSpecificOutput: {
      permissionDecision: 'deny',
      permissionDecisionReason: 'blocked-by-extension-fixture'
    }
  }));
  process.exit(0);
}
process.stdout.write(JSON.stringify({
  hookSpecificOutput: {
    permissionDecision: 'allow',
    updatedInput: { ...(input.tool_input ?? {}), hooked: true },
    additionalContext: 'hook-extension-context'
  }
}));
