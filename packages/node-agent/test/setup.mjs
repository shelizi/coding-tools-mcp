// Test processes must never inherit live Agent state or listener overrides.
delete process.env.CTMCP_DATA_DIR;
delete process.env.CTMCP_CONFIG_FILE;
delete process.env.CTMCP_PORT;
