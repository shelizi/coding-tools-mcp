process.env.CTMCP_SHARED_STORE_DISABLED = '1';

const { runBuffered } = await import('../../dist/processes.js');
for (let index = 0; index < 2; index += 1) {
  const result = await runBuffered(process.execPath, ['-e', ''], process.cwd());
  if (result.code !== 0) throw new Error(`buffered child ${index} exited ${result.code}: ${result.stderr}`);
}
process.stdout.write('standalone-startup-ok\n');
