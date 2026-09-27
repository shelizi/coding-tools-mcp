// Test processes must never inherit live Agent state or listener overrides.
delete process.env.CTMCP_DATA_DIR;
delete process.env.CTMCP_CONFIG_FILE;
delete process.env.CTMCP_PORT;
process.env.CTMCP_SHARED_STORE_DISABLED = '1';

// Windows cancels later tests in a file when spawned children unref every timer
// and the test runner sees beforeExit. A referenced interval keeps the file
// alive; --test-force-exit ends the process after the tests settle.
if (process.platform === 'win32' && process.env.NODE_TEST_CONTEXT === 'child-v8') {
  setInterval(() => {}, 60_000);
}
