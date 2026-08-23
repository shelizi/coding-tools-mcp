import readline from 'node:readline';

const lines = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
for await (const line of lines) {
  if (!line.trim()) continue;
  const request = JSON.parse(line);
  if (request.method === 'notifications/initialized') continue;
  if (request.method === 'initialize') {
    process.stdout.write(`${JSON.stringify({
      jsonrpc: '2.0', id: request.id,
      result: { protocolVersion: '2025-11-25', capabilities: { tools: {} }, serverInfo: { name: 'fixture', version: '1.0.0' } }
    })}\n`);
    continue;
  }
  if (request.method === 'tools/list') {
    process.stdout.write(`${JSON.stringify({
      jsonrpc: '2.0', id: request.id,
      result: { tools: [{ name: 'echo', description: 'Echo a message.', inputSchema: { type: 'object', properties: { message: { type: 'string' } } } }] }
    })}\n`);
    continue;
  }
  if (request.method === 'tools/call') {
    const message = String(request.params?.arguments?.message ?? '');
    process.stdout.write(`${JSON.stringify({
      jsonrpc: '2.0', id: request.id,
      result: { content: [{ type: 'text', text: `fixture:${message}` }], structuredContent: { echoed: message } }
    })}\n`);
    continue;
  }
  process.stdout.write(`${JSON.stringify({ jsonrpc: '2.0', id: request.id, error: { code: -32601, message: 'method not found' } })}\n`);
}
