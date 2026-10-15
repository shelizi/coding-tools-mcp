import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';

const read = relative => readFile(new URL(`../${relative}`, import.meta.url), 'utf8');

test('Docker production switch rebuilds a unique tag then health-checks before moving :local', async () => {
  const [script, pkg] = await Promise.all([
    read('scripts/switch-node-agent-docker.sh'),
    read('package.json')
  ]);
  const scripts = JSON.parse(pkg).scripts;

  assert.match(script, /coding-tools-node-agent:rollback-pre-/);
  assert.match(script, /coding-tools-node-agent:sha-/);
  assert.match(script, /--file packages\/node-agent\/Dockerfile/);
  assert.match(script, /BUILD_GIT_SHA/);
  assert.match(script, /refusing to replace the running container from inside itself/);
  assert.match(script, /removing leftover Created container/);
  assert.match(script, /docker update --restart=no/);
  assert.match(script, /CTMCP_NODE_AGENT_IMAGE="\$image" compose up -d --no-build/);
  assert.match(script, /CTMCP_SWITCH_STATE_FILE/);
  assert.match(script, /new container failed health within/);
  assert.match(script, /rolled back to/);
  assert.match(script, /build_image "\$to_tag"[\s\S]*safe_replace "\$to_tag" 1/);
  assert.match(script, /if ! wait_ready "\$started"; then[\s\S]*restore_previous[\s\S]*docker tag "\$new_tag" "\$stable_image"/);
  assert.match(scripts['node-agent:docker:switch'], /switch-node-agent-docker\.sh/);
  assert.match(scripts['node-agent:docker:switch:rollback'], /--rollback/);
});
