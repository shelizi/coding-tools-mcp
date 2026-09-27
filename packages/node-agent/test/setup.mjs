import { realpathSync } from 'node:fs';
import { tmpdir } from 'node:os';

// Test processes must never inherit live Agent state or listener overrides.
delete process.env.CTMCP_DATA_DIR;
delete process.env.CTMCP_CONFIG_FILE;
delete process.env.CTMCP_PORT;
process.env.CTMCP_SHARED_STORE_DISABLED = '1';

// GitHub Windows runners expose TEMP as an 8.3 path. Canonical comparisons then
// disagree with the short path tests recorded. Point TEMP at the long path.
if (process.platform === 'win32') {
  let longTemp = realpathSync.native(tmpdir());
  if (longTemp.startsWith('\\\\?\\UNC\\')) longTemp = `\\\\${longTemp.slice(8)}`;
  else if (longTemp.startsWith('\\\\?\\')) longTemp = longTemp.slice(4);
  process.env.TEMP = longTemp;
  process.env.TMP = longTemp;
}

// Windows cancels later tests in a file when spawned children unref every timer
// and the test runner sees beforeExit. A referenced interval keeps the file
// alive; --test-force-exit ends the process after the tests settle.
if (process.platform === 'win32' && process.env.NODE_TEST_CONTEXT === 'child-v8') {
  setInterval(() => {}, 60_000);
}
