// Test-only MCP fixture. Never reads a real account, device or filesystem.
const readline = require('node:readline');
const input = readline.createInterface({ input: process.stdin });
let calls = 0;
input.on('line', (line) => {
  let request;
  try { request = JSON.parse(line); } catch { process.exitCode = 2; input.close(); return; }
  if (request.id === undefined) return;
  let result;
  switch (request.method) {
    case 'initialize': result = { protocolVersion: '2025-11-25', capabilities: { tools: {} }, serverInfo: { name: 'stdio-fake', version: '1' } }; break;
    case 'tools/list': result = { tools: [{ name: 'read', description: 'fake read', inputSchema: { type: 'object' } }] }; break;
    case 'tools/call':
      calls += 1;
      result = { content: [{ type: 'text', text: JSON.stringify({ calls, secret: process.env.FAKE_KEY === 'test-value', cleanEnvironment: !process.env.CARGO_MANIFEST_DIR }) }] };
      break;
    case 'ping': result = {}; break;
    default:
      process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, error: { code: -32601, message: 'Method not found' } }) + '\n');
      return;
  }
  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result }) + '\n');
});
